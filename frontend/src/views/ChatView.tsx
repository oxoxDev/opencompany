import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type ReactNode,
  type RefObject,
  type SetStateAction,
} from "react";
import { TriangleAlert } from "lucide-react";
import { toast } from "sonner";

import { listPeople, me as fetchMe, type Person } from "@/api/auth";
import type { OpenCompanyClient } from "@/api/client";
import { deleteTask, type MessageIntent } from "@/api/tasks";
import type { OpenTurn } from "@/lib/live-reply";
import { setInboxEnabled } from "@/api/inbox";
import { uploadChatAttachment } from "@/api/chat";
import { deleteNode, fetchBlobUrl } from "@/api/workspace";
import {
  ApiError,
  type ApprovalSummary,
  type AttachmentDto,
  type GrantScope,
  type TeamMemberDto,
  type TurnStep,
  type Verdict,
  isDetachedChat,
} from "@/api/types";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  fromHistory,
  makeMessage,
  reconcileIds,
  toHostMessageId,
  type ChatMessage,
} from "@/lib/chat";
import { defaultDesks, type Desk } from "@/lib/desks";
import { readLastChannel } from "@/lib/last-channel";
import { readChannelRailCollapsed, writeChannelRailCollapsed } from "@/lib/chat-rail";
import {
  addMemberFailure,
  reportAddMember,
  type AddMemberOutcome,
} from "@/lib/member-feedback";
import { fromDto, newMember, type TeamMember } from "@/lib/team";
import { personAvatar, personName } from "@/lib/person";
import { cn } from "@/lib/utils";
import { useAskerNames } from "@/components/approval-card";
import { useIsDesktop } from "@/hooks/use-mobile";
import { AddMemberDialog, type NewMemberFields } from "./chat/AddMemberDialog";
import { BudgetDialog } from "./chat/BudgetDialog";
import { ChannelRail } from "./chat/ChannelRail";
import { ChatHeader } from "./chat/ChatHeader";
import { MembersPane } from "./chat/MembersPane";
import { TypingLine } from "./chat/TypingLine";
import { MessageComposer } from "./chat/MessageComposer";
import {
  mentionablesFor,
  sameTarget,
  mentionsOutsideChannel,
  utf8ByteLength,
  type Mention,
  type Mentionable,
} from "./chat/mentions";
import { MessageTimeline } from "./chat/MessageTimeline";
import { ThreadPanel } from "./chat/ThreadPanel";
import { useLocalScope } from "@/connections/ConnectionContext";
import {
  buildChannels,
  buildTimeline,
  buildTimelineItems,
  channelIdFromSegment,
  channelMembers,
  channelTitle,
  deskFromDto,
  dmChannelId,
  findChannel,
  firstChannel,
  historyReady,
  HISTORY_UNTRACKED,
  clearTaskCardEverywhere,
  directMessageChannels,
  directMessageForId,
  offersDeliverableChoice,
  resolveDmChannelId,
  toggleReaction,
  type DecidedApproval,
  type HistoryHydration,
  type Transcripts,
} from "./chat/model";

/**
 * The stable empty transcript fallback.
 *
 * `transcripts[channel.id]` can be absent for a channel with no history yet —
 * a newly opened DM, or a desk whose history came back empty. Falling back to a
 * fresh `[]` would give `messages` a new identity on every render, which
 * recomputes the `replyParents`/`loadedMessageIds` memos and re-runs the
 * channel-view effect on every render — and that effect's state write
 * re-renders the shell, closing a render loop. One shared empty array keeps the
 * identity stable until a transcript entry actually lands.
 */
const EMPTY_MESSAGES: ChatMessage[] = [];

interface Props {
  client: OpenCompanyClient;
  company: string | null;
  /** The hash's second segment — the channel id, e.g. `main` in `#/chat/main`. */
  sub: string | null;
  onNavigate: (channelId: string) => void;
  /** Called after a reply lands, so the shell can refresh approvals/status. */
  onReply?: () => void;
  /**
   * Every channel's transcript, keyed by channel id, and its setter — owned by
   * `AppShell` rather than here so a transcript survives this component
   * unmounting when the operator navigates to another view and back (the shell
   * mounts and unmounts `ChatView` per route; component-local state would be
   * discarded on every trip away from Chat).
   */
  transcripts: Transcripts;
  setTranscripts: Dispatch<SetStateAction<Transcripts>>;
  /**
   * How far the shell's rehydration of each channel's history has got, so the
   * timeline can hold a loading state instead of claiming a channel is empty
   * while its history is still on the wire (issue #934). Optional, and absent
   * means "nothing is pending" — a mount with no shell behind it renders as it
   * always did rather than spinning forever.
   */
  hydration?: HistoryHydration;
  /**
   * Called around the awaited chat POST with the **host thread id** it was sent
   * on, so the shell can suppress the SSE echo of our own turn while it is in
   * flight. Without this bracket the shell's live injection and the awaited
   * reply below both render and the bubble doubles — the exact duplicate-bubble
   * race the Conversation surface already brackets against.
   */
  onSendStart?: (threadId: string) => void;
  /**
   * Who is present right now, keyed by user id. Empty when the host has no
   * presence route, or when nobody else is connected to this replica.
   */
  presence?: ReadonlyMap<string, { status: "online" | "away" | "offline" }>;
  /**
   * The company's people, for the members pane's People section.
   *
   * Separate from `members` (teammates) on purpose: desk membership is a
   * teammate concept, and every signed-in person can already see every desk,
   * so people are never "in" or "outside" a channel.
   */
  companyPeople?: Array<{ id: string; label: string }>;
  /**
   * Display names for the typing line, in a stable order — resolved on
   * demand rather than a single precomputed array, because this view needs
   * two independent lines: the main composer's (no `parentId`) and, when a
   * thread is open, that thread's own (`parentId` set). A single `string[]`
   * could only ever answer one of them.
   */
  resolveTypingNames?: (chatId: string, parentId?: string) => string[];
  /** Called as a composer is typed in; the caller throttles. */
  onTyping?: (chatId: string, parentId?: string) => void;
  onSendEnd?: (threadId: string) => void;
  /**
   * The host accepted the turn and answered `202` instead of the reply
   * (issue #983). Distinct from `onSendEnd`, which says the turn is *over*:
   * this one says the POST is over and the turn is not, so the shell keeps the
   * working row up and stops suppressing the live reply frame.
   */
  onSendDetached?: (threadId: string, turnId?: string) => void;
  /**
   * The chat POST **threw** rather than answering (issue #1000).
   *
   * The third outcome, and distinct from `onSendEnd` in the way that matters:
   * `onSendEnd` promises the shell that the reply is already on screen, so the
   * shell may drop the live frame it was holding. A throw promises the
   * opposite — nothing was rendered, and since #983 the turn usually outlives
   * the request that started it, so that held frame is the only copy of the
   * answer anyone is going to get.
   */
  onSendFailed?: (threadId: string) => void;
  /** Called when a delayed response belongs to a previous company scope. */
  onSendStale?: (threadId: string) => void;
  /**
   * The shell's live company ref, so the stale-response check keeps observing
   * company switches after this view unmounts.
   *
   * A component-local ref would freeze at the last render's company once this
   * subtree disappears — exactly when the operator can walk to another view
   * and switch companies mid-POST. The shell owns `companyRef` and updates it
   * on every company change whether or not Chat is mounted, so a send that
   * resolves or rejects after the switch still sees the new scope and is
   * declared stale rather than writing the old company's reply into the new
   * company's transcript.
   */
  /**
   * The latest connection and company scope, updated by the shell while mounted.
   *
   * `client` is part of the scope for a reason codex flagged (P1): the registry's
   * `reseat` path edits a host address by replacing the `OpenCompanyClient` while
   * deliberately preserving the connection id, so `connection` + `company` alone
   * do not move when the host underneath a send changes. Comparing the client
   * instance catches the old host's late completion after reseat.
   */
  scopeRef: RefObject<{ connection: string; company: string | null; client: OpenCompanyClient }>;
  /**
   * Turns accepted but not settled, by host thread id — including ones this
   * console never POSTed, which is what makes the indicator survive a reload.
   */
  openTurns?: Record<string, OpenTurn[]>;
  /**
   * The in-flight tool timeline the shell folds out of the live turn frames,
   * keyed by **host thread id** — so this view has to resolve its channel to a
   * thread to read it (see `activeThreadId`). Covers turns this console never
   * started, which is most of what issue #367 is about.
   */
  liveStepsByThread?: Record<string, TurnStep[]>;
  /** Channel id → unread count, for the rail's badges. Owned by the shell. */
  unread?: Record<string, number>;
  /**
   * Channel id → unread mentions of this person there.
   *
   * A separate badge from `unread`, not a subset of it: unread is derived in
   * this browser, a mention is a durable host-side fact about *you*. See
   * `ChannelRail`'s prop docs for why merging them would be a loss.
   */
  mentions?: Record<string, number>;
  mentionFeedRevision?: number;
  /**
   * Reports the channel actually on screen — which the hash need not name,
   * since it may have been resolved by the first-channel fallback. The shell
   * clears that channel's unread count and remembers it as where an
   * unaddressed line belongs after this view is gone (issue #368).
   *
   * The second argument is whether *this* channel's history is still on the
   * wire. A mention is durable and there is no older-history pagination to
   * recover one — so the shell must not clear a mention for a message it
   * cannot yet prove is on screen, which is exactly the case where this is
   * `true`.
   */
  onChannelViewed?: (
    channelId: string,
    historyPending: boolean,
    mentionFeedRevision?: number,
    /**
     * The loaded transcript's thread replies (`reply id → parent id`), so the
     * reader can defer a thread-reply mention until its thread is open.
     */
    replyParents?: ReadonlyMap<string, string>,
    /** The thread panel currently open, or `null`. */
    openThreadId?: string | null,
    /** All loaded message ids for this channel, so the reader can defer a
     * mention whose subject is outside the history window. */
    loadedMessageIds?: ReadonlySet<string>,
  ) => void;
  /**
   * Every approval currently awaiting the operator, straight off the shell's
   * feed, plus the host thread → channel map that places them (#379).
   *
   * Passed whole and filtered here rather than pre-filtered upstream, because
   * the filter needs the channel actually on screen — which this view resolves,
   * not the shell. An approval whose `thread` names no channel this company has
   * (a workflow delivery, a scheduler tick, anything parked before #379) simply
   * matches nothing and stays on the Approvals page, which is the whole
   * additive contract.
   */
  approvals?: ApprovalSummary[];
  chatChannelByThread?: Record<string, string>;
  /** Now, for a card's "waiting N minutes" line. */
  now?: number;
  /**
   * Decide an approval from inside the conversation. Owned by the shell so the
   * witnessed verdict survives this view unmounting — the operator can walk to
   * Approvals and back mid-turn.
   */
  onDecideApproval?: (approval: ApprovalSummary, verdict: Verdict, scope: GrantScope) => void;
  /** The verdict each card is waiting on, and the ones already witnessed. */
  decidingApprovals?: ReadonlyMap<string, Verdict>;
  decidedApprovals?: Record<string, DecidedApproval>;
  /**
   * Decisions that did not land, per approval id (#842) — the message to show
   * on that item. Owned by the shell, like the two maps above, because a
   * failure has to outlive this view unmounting: the operator's next move after
   * one is often to open the Approvals page and come back.
   */
  failedApprovals?: Record<string, string>;
}

const FIRST_TEAM_BRIEF =
  "Help us get started: propose the first three priorities for our company and who should own each one.";

/**
 * The chat workspace.
 *
 * One screen replaces what used to be three: the Conversation page's thread
 * list, the Team page's roster, and the desks those two shared without ever
 * being connected. Here the desks are channels, every teammate has a DM, and
 * the roster sits in a pane you can open beside the transcript.
 *
 * Every channel posts to the same company chat endpoint — a channel scopes a
 * transcript and fixes the company side's identity, it is not a separate
 * backend. Threads and reactions are console-local for the same reason: the
 * host has no surface for either yet.
 */
export function ChatView({
  client,
  company,
  sub,
  onNavigate,
  onReply,
  transcripts,
  setTranscripts,
  hydration = HISTORY_UNTRACKED,
  onSendStart,
  presence,
  companyPeople,
  resolveTypingNames,
  onTyping,
  onSendEnd,
  onSendDetached,
  onSendFailed,
  onSendStale,
  scopeRef,
  openTurns,
  liveStepsByThread,
  unread,
  mentions,
  mentionFeedRevision,
  onChannelViewed,
  approvals,
  chatChannelByThread,
  now,
  onDecideApproval,
  decidingApprovals,
  decidedApprovals,
  failedApprovals,
}: Props) {
  // Which (connection, company) this subtree's browser-local state belongs to.
  const scope = useLocalScope();
  const [members, setMembers] = useState<TeamMember[]>([]);
  const [loadingTeam, setLoadingTeam] = useState(true);
  const [fromHost, setFromHost] = useState(false);
  /**
   * The company's channels — `null` until `/desks` has answered.
   *
   * Seeding this with `defaultDesks()` is what made every deep link flash
   * `#general`: the first render of every mount resolved the hash against the
   * fabricated `main`/`strategy`/`creative`/`frontdesk` set, then swapped under
   * the operator once the real desks landed (issue #370). `null` means "not
   * answered yet", so nothing resolves against a set the company doesn't have.
   */
  const [desks, setDesks] = useState<Desk[] | null>(null);
  /** Set when `/desks` failed for a reason that isn't "this host has none". */
  const [desksError, setDesksError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [composerPrefill, setComposerPrefill] = useState<{
    text: string;
    revision: number;
  } | null>(null);
  const [openThreadId, setOpenThreadId] = useState<string | null>(null);
  const [dismissingCardId, setDismissingCardId] = useState<string | null>(null);
  const [membersOpen, setMembersOpen] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  const [mobilePane, setMobilePane] = useState<"rail" | "chat">("chat");
  // Whether the transcript is actually on screen. At `lg` (≥1024) the rail and
  // transcript share the viewport (`hidden lg:flex`), so it is visible even
  // while `mobilePane` says "rail"; below that the pane toggle is the whole
  // story. Mention clearing is gated on this so a mention cannot be marked
  // read while only the rail is showing (codex P1 review).
  const isDesktop = useIsDesktop();
  const chatPaneVisible = mobilePane === "chat" || isDesktop;
  const [channelsCollapsed, setChannelsCollapsed] = useState(() => readChannelRailCollapsed(scope));
  // Section disclosure is shared by the desktop and sub-`lg` rail instances
  // (codex P2 review): each instance would otherwise keep its own fold state,
  // so dropping below `lg` reopened every section the operator had folded.
  const [railOpenSections, setRailOpenSections] = useState<Record<string, boolean>>({});
  const toggleRailSection = (id: string) =>
    setRailOpenSections((prev) => ({ ...prev, [id]: !(prev[id] ?? true) }));
  // The header's density toggle stays mounted across a collapse/expand, but the
  // compact rail's expand button does not — expanding unmounts it while a
  // keyboard user is still focused on it, dropping them at the document. The
  // ref lets the expand action hand focus to the header toggle instead (the
  // fix for the rail's issue #1340 focus review).
  const channelsToggleRef = useRef<HTMLButtonElement>(null);
  const [isAdmin, setIsAdmin] = useState(false);
  /** Your own avatar reference, once `loadViewer` has resolved who you are. */
  const [youAvatar, setYouAvatar] = useState<string | undefined>(undefined);
  // Who set which cap (issue #360, ported from the retired Team page). Only
  // an admin may read the user directory, so this stays empty for a member —
  // the attribution line degrades to "an admin" rather than disappearing.
  const [people, setPeople] = useState<Person[]>([]);
  // The member whose budget dialog is open, if any.
  const [budgetFor, setBudgetFor] = useState<TeamMember | null>(null);

  // A host switch keeps this mounted briefly, so replace rather than carry the
  // previous connection's layout preference into the next company.
  useEffect(() => {
    setChannelsCollapsed(readChannelRailCollapsed(scope));
  }, [scope]);

  function toggleChannels() {
    setChannelsCollapsed((collapsed) => {
      const next = !collapsed;
      writeChannelRailCollapsed(scope, next);
      // Expanding from the compact rail unmounts the button that carried focus;
      // hand it to the header toggle, which is mounted on both density states.
      // `next` is the rail's new collapsed state, so expanding is `!next` —
      // collapsing from the header's own toggle leaves that button mounted,
      // and the focus it already holds is the right place to stay.
      if (!next) channelsToggleRef.current?.focus();
      return next;
    });
  }

  const boot = useCallback(async () => {
    try {
      const roster = await client.listTeam(company);
      if (roster.length) {
        setMembers(roster.map(fromDto));
        setFromHost(true);
      } else {
        // Nobody, rather than a fabricated roster. A DM list of twelve invented
        // teammates offers conversations with agents the host has never heard
        // of, and the first message to one goes nowhere
        // (`docs/spec/runtime/company-setup.md`).
        setMembers([]);
        setFromHost(false);
      }
    } catch {
      // The roster read failed, so we do not know who works here. Still nobody:
      // guessing a team is what this change exists to stop.
      setMembers([]);
      setFromHost(false);
    } finally {
      setLoadingTeam(false);
    }
  }, [client, company]);

  /**
   * Hiding the budget controls from a non-admin is **courtesy, not
   * enforcement**. The host refuses the write with a 403 whatever this says;
   * showing an operator a control they cannot use is the only thing this
   * prevents.
   */
  // Only the newest load may write, exactly as `loadDesks` guards its own runs.
  // `fetchMe` and `listPeople` can overlap — a scope change while a request is
  // merely slow — and a stale answer landing last would wear the previous
  // company's face on your own lines. The face is cleared *before* the fetch so
  // a slow request can never pin an old avatar across a scope change; the
  // timeline falls back to the name-seeded mascot meanwhile.
  const viewerRun = useRef(0);
  const loadViewer = useCallback(async () => {
    const run = ++viewerRun.current;
    setYouAvatar(undefined);
    let admin = false;
    try {
      const who = await fetchMe(client, company);
      if (run !== viewerRun.current) return;
      admin = who.role === "admin";
      // Your own face, so your lines in a busy channel are yours at a glance.
      // Read from the same call that resolves your role — there is no second
      // round trip for it, and no way for the two to disagree about who you are.
      setYouAvatar(personAvatar(who));
    } catch {
      if (run !== viewerRun.current) return;
      // No user plane on this host, or not signed in — treat as non-admin, and
      // leave the composer's own lines on the name-seeded fallback.
    }
    setIsAdmin(admin);
    if (!admin) {
      setPeople([]);
      return;
    }
    try {
      const people = await listPeople(client, company);
      if (run !== viewerRun.current) return;
      setPeople(people);
    } catch {
      if (run !== viewerRun.current) return;
      // Attribution falls back to "an admin"; not worth a toast.
      setPeople([]);
    }
  }, [client, company]);

  useEffect(() => {
    setLoadingTeam(true);
    void boot();
    void loadViewer();
  }, [boot, loadViewer]);

  /** A human label for whoever set a cap — never a raw user id. */
  function whoSet(userId: string): string {
    const person = people.find((p) => p.id === userId);
    return person ? personName(person) : "an admin";
  }

  const budgetError = (error: unknown, fallback: string): string => {
    if (error instanceof ApiError) {
      if (error.status === 404) return "This host doesn't support console budgets yet.";
      return error.message;
    }
    return error instanceof Error ? error.message : fallback;
  };

  /**
   * Set, change, or remove a teammate's daily cap.
   *
   * `cap` is `null` to remove the cap and a number to set one — `0` included,
   * which caps the teammate at nothing. The two are different states on the
   * host and must stay different here, which is why this takes `number |
   * null` and never an optional.
   */
  async function applyBudget(member: TeamMember, cap: number | null) {
    try {
      const row = await client.setTeamBudget(member.id, cap, company);
      // Update the one card from the host's answer rather than refetching the
      // roster: the response IS the new state, so a refetch could only disagree.
      setMembers((ms) => ms.map((m) => (m.id === member.id ? { ...m, ...fromDto(row) } : m)));
      toast.success(cap === null ? "Daily cap removed." : `Daily cap set to $${cap.toFixed(2)}.`);
    } catch (error) {
      toast.error(budgetError(error, "Couldn't change the daily cap."));
    }
  }

  /** Drop the override so the company's own default applies again. */
  async function resetBudget(member: TeamMember) {
    try {
      const row = await client.clearTeamBudgetOverride(member.id, company);
      setMembers((ms) => ms.map((m) => (m.id === member.id ? { ...m, ...fromDto(row) } : m)));
      toast.success("Reset to the company default.");
    } catch (error) {
      toast.error(budgetError(error, "Couldn't reset the daily cap."));
    }
  }

  // Only the newest load may write. Two loads can be in flight at once — a
  // company switch, or a Retry over a request that is merely slow rather than
  // dead — and a stale answer landing last would replace the current company's
  // channels with the previous one's.
  const desksRun = useRef(0);

  /**
   * The company's real desks, when the host exposes them — a company with its
   * own desks gets its own channels instead of the generic strategy/creative/
   * front-desk trio.
   *
   * Two outcomes are *not* failures and fall back to the static defaults: a
   * host with no `.../desks` route at all (404, the pre-#53 shape the
   * Conversation path also tolerates) and a host that answers with no desks.
   * Both mean "this company has no desks surface", which the defaults exist
   * for. Anything else — a 500, a timeout, an offline tab — is a genuine
   * failure, and pinning the fabricated desks on top of it is what made a
   * broken `/desks` permanently show `#general` while the URL claimed a real
   * desk (issue #370). Those surface as an error the operator can retry.
   */
  const loadDesks = useCallback(async () => {
    const run = ++desksRun.current;
    setDesks(null);
    setDesksError(null);
    try {
      const dtos = await client.listDesks(company);
      if (run !== desksRun.current) return;
      setDesks(dtos.length ? dtos.map(deskFromDto) : defaultDesks());
    } catch (error) {
      if (run !== desksRun.current) return;
      if (error instanceof ApiError && error.status === 404) {
        setDesks(defaultDesks());
        return;
      }
      setDesksError(
        error instanceof Error ? error.message : "Couldn't load this company's channels.",
      );
    }
  }, [client, company]);

  useEffect(() => {
    void loadDesks();
  }, [loadDesks]);

  /**
   * Re-entering Chat with no channel in the hash returns the operator to the
   * one they were last reading (issue #412).
   *
   * Leaving Chat drops the hash's second segment, so coming back used to fall
   * straight through to `firstChannel` — which is not memory, it is whichever
   * channel sorts first, and it cost a re-navigation on every trip.
   *
   * The remembered id is written into the **hash**, not held here, for three
   * reasons: the channel on screen stays shareable, it survives a reload, and a
   * remembered channel that has since been removed then falls through the exact
   * same stale-id path as a bad deep link — so it raises the unknown-channel
   * notice from issue #370 rather than needing a second one, and lands on the
   * fallback visibly rather than silently. The `onChannelViewed` report for
   * whatever it fell back to re-remembers that instead, so a vanished channel
   * corrects itself after one visit.
   *
   * A hash that already names a channel is a deep link and always outranks
   * memory; this only runs when there is nothing to override. The ref makes it
   * one attempt per bare-hash entry, so it can never fight a navigation.
   */
  const restoredFor = useRef<string | null | undefined>(undefined);
  useEffect(() => {
    if (sub) {
      // A channel is named, so the next bare `#/chat` is a fresh re-entry.
      restoredFor.current = undefined;
      return;
    }
    // Scoped like `readLastChannel(scope)`: two connections serving the same
    // company must each restore their own remembered channel, so a host switch
    // cannot be mistaken for a re-entry into the previous host's state.
    const scopeKey = `${scope.connection}::${scope.company ?? "single"}`;
    if (restoredFor.current === scopeKey) return;
    restoredFor.current = scopeKey;
    const remembered = readLastChannel(scope);
    if (remembered) onNavigate(remembered);
  }, [company, scope, sub, onNavigate]);

  // No channels exist until the host has answered. Resolving against a
  // half-built list is exactly the first-paint swap issue #370 describes.
  //
  // The shell's live scope ref, not a local one: a local ref would freeze at
  // the last render's scope once this subtree unmounts, and a `client.chat`
  // still in flight from before the switch would then pass its stale check and
  // write the old company's reply into the new company's transcript. The shell
  // keeps updating its ref on every connection/company change, mounted or not,
  // so the comparison in `send` stays honest after Chat is gone (codex P1).

  const sections = useMemo(
    () => (desks ? buildChannels(members, desks, transcripts) : []),
    [members, desks, transcripts],
  );
  // The hash's channel, else the first one that exists. There used to be a
  // literal "main" between the two — an id only the *fallback* desks carry, so
  // it matched nothing once a company's real desks loaded and matched the same
  // channel `firstChannel` returns when they hadn't. It never selected anything
  // the line below wouldn't; it only made "main" look like a real channel id
  // (issue #368).
  /**
   * A `#/chat/dm:…` link minted before issue #364 re-keyed DMs onto the
   * teammate's id, mapped onto the id that channel has now.
   *
   * One release of grace for a bookmarked or pasted link. Resolution only —
   * nothing is ever addressed or stored under the old id, so this shim can be
   * deleted without leaving anything stranded.
   */
  const decodedSub = channelIdFromSegment(sub);
  const resolvedSub =
    decodedSub && !findChannel(sections, decodedSub)
      ? resolveDmChannelId(decodedSub, members)
      : null;
  /**
   * The channel the hash names, else the first one that exists.
   *
   * The rail only carries DMs with a transcript (issue #1335), so `findChannel`
   * answers `null` for an inactive DM — but `dm:<teammate-id>` is still a
   * valid, directly-addressable conversation. `directMessageForId` is the
   * all-roster resolver that keeps such a deep link (and the New message
   * picker's selection) landing on the DM before the first-channel fallback
   * takes over, without ever adding the inactive DM to the rail.
   */
  const channel = desks
    ? (findChannel(sections, resolvedSub ?? decodedSub) ??
      directMessageForId(members, resolvedSub ?? decodedSub) ??
      firstChannel(sections))
    : null;
  /**
   * The hash named a channel this company doesn't have, and the first-channel
   * fallback answered instead.
   *
   * Only meaningful once the desks are in: before that, *every* id looks
   * unknown. Derived rather than stored, so it clears itself the moment the
   * hash changes — there is no stale banner to dismiss. A legacy DM link that
   * the shim above resolved is not unknown; it found its channel.
   *
   * An inactive DM is not unknown either: the rail only carries DMs with a
   * transcript (issue #1335), so `findChannel` answers `null` for a DM the
   * picker just opened, but `directMessageForId` still resolves it against the
   * whole roster. Check that resolver explicitly rather than leaning on
   * `resolvedSub`, whose legacy-id shim is meant to be deletable.
   */
  const unknownChannel =
    desks &&
    decodedSub &&
    !resolvedSub &&
    !findChannel(sections, decodedSub) &&
    !directMessageForId(members, decodedSub)
      ? decodedSub
      : null;

  /**
   * Who is in the channel on screen — `null` when it names no membership, in
   * which case the pane falls back to the whole roster (issue #369).
   *
   * A desk's membership comes from the host. A DM's is the one teammate on the
   * other end: it has no `memberIds` (nothing in the model claims a DM has a
   * roster), so the two-person case is stated here rather than faked upstream.
   */
  const inChannel = useMemo(() => {
    if (!channel) return null;
    if (channel.kind === "dm") return channel.member ? [channel.member] : null;
    return channelMembers(channel, members);
  }, [channel, members]);

  /**
   * Everything an `@` can name in this company.
   *
   * Fetched once per company rather than per channel: the directory is
   * company-wide, and only the `inChannel` ranking below is per channel.
   *
   * A host that predates the route answers 404, which lands here as `null` —
   * read as "no picker", so typing an `@` stays plain text and the host still
   * extracts what it can. An older host therefore degrades to the composer's
   * previous behaviour rather than to a broken one.
   */
  const [directory, setDirectory] = useState<Mentionable[] | null>(null);
  /**
   * One epoch token shared by the mount fetch and `reloadDirectory`. Every
   * fetch bumps it and applies its response only while the token is still
   * current, so a fetch superseded by a company switch (or by a second roster
   * write) cannot land after the newer directory and hand the picker stale —
   * possibly cross-company — rows. Selecting a row the server will demote is a
   * bad row, so advertising it in the first place is what the guard prevents.
   */
  const directoryEpoch = useRef(0);
  /**
   * Re-read the mention directory.
   *
   * Called on mount and after a roster write, so a teammate added here appears
   * in the picker at once and one removed does not stay selectable until the
   * next reload (server revalidation would demote it, but offering a row that
   * can only fail is a bad row).
   */
  const reloadDirectory = useCallback(() => {
    const epoch = ++directoryEpoch.current;
    void client
      .mentionables(company)
      .then((d) => {
        if (epoch === directoryEpoch.current) setDirectory(mentionablesFor(d));
      })
      .catch(() => {
        if (epoch === directoryEpoch.current) setDirectory(null);
      });
  }, [client, company]);
  useEffect(() => {
    const epoch = ++directoryEpoch.current;
    setDirectory(null);
    void client
      .mentionables(company)
      .then((d) => {
        if (epoch === directoryEpoch.current) setDirectory(mentionablesFor(d));
      })
      .catch(() => {
        if (epoch === directoryEpoch.current) setDirectory(null);
      });
    return () => {
      directoryEpoch.current += 1;
    };
  }, [client, company]);

  /**
   * The directory with this channel's teammates marked, so they rank first.
   *
   * Re-marked rather than re-fetched on a channel switch — the rows are the
   * same, only their ordering hint changes.
   */
  const mentionables = useMemo(() => {
    if (!directory) return undefined;
    const inside = new Set((inChannel ?? []).map((m) => m.id));
    return directory.map((entry) =>
      entry.target.kind === "agent"
        ? { ...entry, inChannel: inside.has(entry.target.id) }
        : entry,
    );
  }, [directory, inChannel]);

  const outsideChannel = useMemo(() => {
    if (!inChannel) return members;
    const inside = new Set(inChannel.map((m) => m.id));
    return members.filter((m) => !inside.has(m.id));
  }, [inChannel, members]);

  const messages = useMemo(
    () => (channel ? (transcripts[channel.id] ?? EMPTY_MESSAGES) : EMPTY_MESSAGES),
    [transcripts, channel?.id],
  );
  /**
   * Whether this channel's history is still on the wire.
   *
   * `messages` cannot tell you — the `?? []` above collapses "never fetched"
   * into "empty", which is why the timeline could claim a reloaded DM was brand
   * new (issue #934). The roster is part of the answer too: a DM's channel only
   * exists once `members` lands, and until then nothing has even asked the shell
   * to hydrate it.
   */
  const historyPending = channel
    ? loadingTeam || !historyReady(hydration, channel.id)
    : false;
  const entries = useMemo(
    () => (channel ? buildTimeline(messages, channel, members, youAvatar) : []),
    [messages, channel, members, youAvatar],
  );
  /**
   * The open channel's thread replies, for the mention-clearing gate: a reply
   * is folded out of the main timeline (`buildTimeline`), so a mention inside
   * one must not clear on channel-open alone — only once the thread panel
   * actually renders it. Keyed by the console's `h<seq>` id, the namespace
   * `subjectId` on a mention notification meets through `hostMessageId`.
   */
  const replyParents = useMemo(() => {
    const map = new Map<string, string>();
    for (const m of messages) {
      if (m.parentId) map.set(m.id, m.parentId);
    }
    return map;
  }, [messages]);

  /** All loaded message ids in this channel, for the mention-clearing gate. */
  const loadedMessageIds = useMemo(() => new Set(messages.map((m) => m.id)), [messages]);

  /**
   * The approvals raised in the channel on screen (#379).
   *
   * **Derived, never appended.** The cards come from server state on every
   * render, so a pending one survives a reload — better than the transcripts it
   * sits among, which are still console-local (#364), and deliberately not
   * dependent on that being fixed.
   *
   * An approval with no `thread`, or one naming a thread this company has no
   * channel for, resolves to `null` and matches nothing. That is how a workflow
   * delivery or a scheduler tick stays Approvals-page-only.
   *
   * `desks` gates the derivation for the same reason it gates the channel list
   * (#393): before `/desks` answers, every thread id looks unknown, and placing
   * cards against a half-built channel set is the first-paint swap #370
   * describes one surface over.
   */
  const channelApprovals = useMemo(() => {
    if (!desks || !channel || !approvals?.length) return [];
    const byThread = chatChannelByThread ?? {};
    return approvals.filter((a) => a.thread && byThread[a.thread] === channel.id);
  }, [desks, channel, approvals, chatChannelByThread]);

  /**
   * Cards the operator has already decided, which the feed no longer carries.
   *
   * The host drops a resolved approval from `GET …/approvals` at once, so these
   * cannot be re-derived from `approvals` — they come from the shell's witnessed
   * map, which keeps the last-seen summary precisely so a decided card can
   * settle in place rather than blinking out of the thread.
   */
  const settledApprovals = useMemo(() => {
    if (!decidedApprovals || !channel || !desks) return [];
    const live = new Set(channelApprovals.map((a) => a.id));
    const byThread = chatChannelByThread ?? {};
    return Object.values(decidedApprovals)
      .map((d) => d.approval)
      .filter((a) => !live.has(a.id))
      .filter((a) => a.thread && byThread[a.thread] === channel.id);
  }, [decidedApprovals, channel, desks, channelApprovals, chatChannelByThread]);

  const askerNames = useAskerNames(client, company, channelApprovals);

  const items = useMemo(
    () =>
      buildTimelineItems(
        entries,
        [...channelApprovals, ...settledApprovals],
        decidedApprovals ?? {},
      ),
    [entries, channelApprovals, settledApprovals, decidedApprovals],
  );

  // An open thread only makes sense while its parent is on screen; switching
  // channels closes it rather than leaving a panel pointing at nothing.
  useEffect(() => {
    setOpenThreadId(null);
  }, [channel?.id]);

  // Whoever owns the unread counts needs to know what is actually being looked
  // at. Re-runs as the open channel's transcript grows, not only on a switch:
  // a reply that lands while you are reading the channel is read, and should
  // not leave a badge on the channel you are sitting in. It also re-runs when
  // a thread opens or closes: opening a thread renders its replies, so the
  // replies' mentions — which the channel-open alone must not clear — clear
  // the moment the thread makes them visible.
  //
  // Gated on the transcript actually being on screen: below `lg`, `mobilePane
  // === "rail"` hides the pane, and a mention that lands while the operator is
  // only looking at the channel rail must not be marked read behind their back.
  // The gate itself is a dependency, so re-opening the pane from the rail
  // re-runs the report and clears whatever is newly visible.
  useEffect(() => {
    if (channel && chatPaneVisible)
      onChannelViewed?.(
        channel.id,
        historyPending,
        mentionFeedRevision,
        replyParents,
        openThreadId,
        loadedMessageIds,
      );
  }, [
    channel?.id,
    messages.length,
    historyPending,
    mentionFeedRevision,
    onChannelViewed,
    replyParents,
    openThreadId,
    loadedMessageIds,
    chatPaneVisible,
  ]);

  // Upload one attachment's bytes for the composer (issue #1682). Bound to the
  // active connection's client/company so the composer stays agnostic of both.
  // Must live above the early returns: a hook after them is skipped on the
  // loading render and present on the next, which is a Rules-of-Hooks crash
  // ("Rendered more hooks than during the previous render").
  const uploadAttachment = useCallback(
    (file: File) => uploadChatAttachment(client, company, file),
    [client, company],
  );

  // Fetch a stored attachment's bytes as an object URL for the transcript
  // (issue #1682). The blob route needs the client's bearer, which an `<img>`
  // or a bare link cannot carry — so the row resolves through this and the
  // caller revokes the URL when done. Reuses the hardened `/workspace/blob`
  // serve untouched. The optional `signal` lets a preview that scrolls out of
  // view cancel its in-flight download (codex review finding).
  const resolveAttachmentUrl = useCallback(
    (nodeId: string, signal?: AbortSignal) =>
      fetchBlobUrl(client, company, nodeId, signal),
    [client, company],
  );

  /**
   * Delete an uploaded-but-never-sent attachment's workspace node (issue
   * #1682, codex review finding).
   *
   * A staged file is uploaded (and charged against the workspace quota) the
   * moment it lands, before the operator has sent anything — replacing it,
   * removing it, or leaving the composer used to just drop the local
   * reference, leaving the binary node on the server forever. Bound the same
   * way `uploadAttachment` is; best-effort, since a failed cleanup here must
   * never block the operator from continuing to compose.
   */
  const deleteAttachment = useCallback(
    (nodeId: string) => {
      void deleteNode(client, company, nodeId).catch(() => {
        // Best-effort: an orphaned node here is a quota nuisance, not a
        // correctness bug, and the operator has already moved on.
      });
    },
    [client, company],
  );

  // Three ways to have no channel on screen, which used to be one blank pane.
  // Which one it is, is the whole point: "still loading" and "this company has
  // nothing" are different facts and only one of them is worth acting on.
  if (desksError) {
    return (
      <EmptyPane
        title="Couldn't load this company's channels"
        body={desksError}
        action={{ label: "Retry", onClick: () => void loadDesks() }}
      />
    );
  }
  if (!desks) return <LoadingPane />;
  if (!channel) {
    return (
      <EmptyPane
        title="No channels yet"
        body="This company has no desks and nobody on its roster, so there is nothing to talk to. Add a teammate and their direct message shows up here."
        action={{ label: "Add a teammate", onClick: () => setAddOpen(true) }}
        after={
          <AddMemberDialog
            open={addOpen}
            onOpenChange={setAddOpen}
            onAdd={(fields) => void addMember(fields)}
          />
        }
      />
    );
  }
  // A local the closures below can capture as non-null: TypeScript hoists
  // function declarations, so the guard above does not narrow inside them.
  const active = channel;
  // The host thread this channel is addressed on. A real desk channel's id
  // doubles as its thread id (`deskFromDto`), so addressing by it routes to
  // that desk's lead. A DM's id is console-local (`dmChannelId`), not a host
  // thread — but `chat` also accepts a roster teammate id directly
  // (`responder_for` in `src/harness/brain.rs`), which is exactly what a DM's
  // `member.id` is, so a DM addresses that teammate the same way a desk
  // addresses its lead. It is also the id every live turn frame carries.
  const activeThreadId = active.kind === "channel" ? active.id : active.member?.id;
  const liveSteps = activeThreadId ? liveStepsByThread?.[activeThreadId] : undefined;
  /**
   * The turn this channel is waiting on, if any (issue #983).
   *
   * Sourced from the shell rather than from local `sending`, which is the whole
   * point: `sending` only knows about a POST *this* component made, so it went
   * false on every reload and on every walk to another view. An open turn is a
   * fact about the company, so the indicator survives both.
   */
  const openTurn = activeThreadId ? openTurns?.[activeThreadId]?.[0] : undefined;
  /**
   * The count beside the channel title.
   *
   * A DM is stated as 2 rather than derived: it is a two-person conversation,
   * but the operator has no roster row, so counting rows would say 1 and
   * inventing a "You" row to make the arithmetic work would be worse. A desk
   * counts its own members; a channel with no membership of its own still
   * counts the company, which is all it can honestly claim.
   */
  const headerCount = active.kind === "dm" ? 2 : (inChannel?.length ?? members.length);
  /**
   * The teammate on the other end of this DM exists only in the console (issue
   * #364) — a starter-roster row, or one added while the host had no team write
   * plane.
   *
   * Worth saying out loud, and worth being precise about *what* is local. The
   * transcript is not: the DM is addressed by a reload-stable id now, so the
   * host journals it and gives it back. What is missing is the other half of the
   * conversation — there is no such agent on the company, so nothing on the
   * roster answers. Claiming the whole channel was console-local would be the
   * old, wrong story; saying nothing would leave an operator waiting for a reply
   * that is never coming.
   */
  const consoleOnlyMember =
    active.kind === "dm" && !fromHost && active.member ? active.member.name : null;

  const append = (channelId: string, ...added: ChatMessage[]) =>
    setTranscripts((t) => ({ ...t, [channelId]: [...(t[channelId] ?? []), ...added] }));

  /**
   * Post a line and thread the company's answer back into the same place.
   * `parentId` set means the exchange stays inside the thread panel.
   *
   * The thread is now a fact about the transcript, not about this browser: the
   * parent goes to the host with the message, and both halves come back under
   * it on the next reload (issue #364). A parent that has no durable id yet is
   * dropped from the request rather than sent as a local counter the host
   * cannot resolve — the row's own actions are disabled in that window, so this
   * is the belt to that brace.
   *
   * Returns whether the POST reached the host and journaled (codex review
   * round 4, on top of round 2's naive version): `true` for every outcome
   * where `client.chat` itself resolved (a normal reply, a detached turn, or
   * a stale response the caller discards) — the host answered, so the
   * journal write is a fact. `undefined` when the request THREW, because a
   * throw is genuinely ambiguous: `accept_chat_turn` journals the message
   * before the turn's cycle is spawned onto its own task, and a synchronous
   * (non-detached) send then awaits that task — so a failure surfacing from
   * deep in cycle execution reaches this `catch` looking identical to one
   * that never reached the journal at all. There is no `false` this function
   * ever returns: nothing observable here tells "refused before journal"
   * and "journaled, then the turn itself failed" apart. The composer treats
   * `undefined` as "unknown — leave it alone", never as "not sent"; see
   * `deleteAttachment` and `MessageComposer.send`.
   */
  async function send(
    text: string,
    intent?: MessageIntent,
    parentId?: string,
    attachments?: AttachmentDto[],
    mentions?: Mention[],
  ): Promise<boolean | undefined> {
    // The one genuinely safe `false`: another send is already in flight, so
    // this call's own text/attachments were never handed to `client.chat` at
    // all — no server round trip happened for them, no ambiguity possible.
    if (sending) return false;
    const scopeAtSend = {
      connection: scope.connection,
      company: scope.company,
      client,
    };
    const target = active.id;
    const chatId = activeThreadId;
    // The optimistic bubble carries the attachments too, so the operator sees
    // the file on their own message the instant they send (issue #1682) — the
    // SSE echo / reload copy then matches it, projected from the same durable
    // references the host resolved.

    // Warn about @-mentioning a teammate who is not on this channel.
    if (mentions?.length && inChannel) {
      const channelIds = inChannel.map((m) => m.id);
      const outside = mentionsOutsideChannel(mentions, channelIds);
      if (outside.length) {
        toast.warning(
          outside.length === 1
            ? "A teammate you @-mentioned is not on this channel — they won't see the message."
            : `${outside.length} teammates you @-mentioned are not on this channel — they won't see the message.`,
        );
      }
    }

    // Chips need a label and a `mine` flag, which the wire mention (target +
    // span) does not carry; the directory supplies the label. The optimistic
    // row is never replaced by history — the id reconcile below makes the
    // durable row "known", so `hydrateChannel` skips it — which means the
    // metadata has to land on this row or the just-sent line renders without
    // chips until reload.
    const localMentions = mentions?.map((m) => {
      // `sameTarget` rather than an inline comparison: narrowing one operand
      // says nothing about the other, so the inline form does not typecheck —
      // and the rule belongs in one place regardless.
      const row = mentionables?.find((e) => sameTarget(e.target, m.target));
      return {
        text: m.text,
        // Incoming/rendered mention offsets use the host's UTF-8 byte contract;
        // the composer keeps UTF-16 offsets only while editing.
        offset: utf8ByteLength(text.slice(0, m.offset)),
        label: row?.label ?? m.text,
        // `@everyone` addresses the room, the author included; a pick of a
        // teammate or person names somebody else.
        mine: m.target.kind === "everyone",
      };
    });
    const local = makeMessage("you", text, {
      parentId,
      attachments,
      mentions: localMentions?.length ? localMentions : undefined,
    });
    append(target, local);
    setSending(true);
    // Claim the thread for the duration of the POST. The backend journals an
    // `AgentReply` for our own turn too and pushes it over SSE mid-await, so
    // without this the shell injects that echo *and* the awaited reply lands
    // below — two bubbles for one turn.
    if (chatId) onSendStart?.(chatId);
    // Which of the POST's three outcomes actually happened, decided here and
    // reported once in the `finally`. Only `"resolved"` means the reply is on
    // screen; the other two leave a turn running on the host and the stream as
    // the delivery path, so telling the shell "ended" for either would take the
    // working row down mid-turn (detached) or throw away the reply it was
    // holding (failed). See `PendingSyncPosts` for the table.
    let outcome: "resolved" | "detached" | "failed" | "stale" = "resolved";
    try {
      const answer = await client.chat(
        text,
        company,
        chatId,
        toHostMessageId(parentId),
        intent,
        // Ask for the turn's id rather than its answer. A host that predates the
        // field ignores this and answers synchronously, which is why the branch
        // below reads the response's shape and never this argument.
        true,
        // Node ids only (issue #1682): the host re-resolves each against this
        // company's workspace and takes the name/mime/size from the store, so
        // the client neither sends nor is trusted for that metadata.
        attachments?.map((a) => a.nodeId),
        // Who the picker resolved. The host re-validates every entry and
        // demotes what no longer exists, so this is a suggestion; omitting it
        // asks the host to extract from the text instead.
        //
        // The composer tracks offsets as UTF-16 indices (they drive textarea
        // and reconcile operations); the host reads them as UTF-8 bytes, so
        // each is converted to the byte length of its prefix here.
        mentions?.map((m) => ({
          ...m,
          offset: utf8ByteLength(text.slice(0, m.offset)),
        })),
      );
      const latestScope = scopeRef.current;
      if (
        latestScope &&
        (scopeAtSend.company !== latestScope.company ||
          scopeAtSend.connection !== latestScope.connection ||
          scopeAtSend.client !== latestScope.client)
      ) {
        outcome = "stale";
        if (chatId) onSendStale?.(chatId);
        // The POST itself succeeded and journaled — this branch only
        // discards the reply because the scope moved on, so anything the
        // request carried (an attachment among them) is durably claimed.
        return true;
      }
      // Reconcile the optimistic id first, for BOTH shapes. On the detached one
      // this is strictly better than what came before: since #983 the message is
      // journaled at accept time, so its durable id is a fact within
      // milliseconds instead of after the whole turn — the bubble becomes
      // replyable and reactable immediately rather than at settle.
      if (answer.messageId) {
        setTranscripts((t) => ({
          ...t,
          [target]: reconcileIds(t[target] ?? [], local.id, answer.messageId!),
        }));
      }
      if (isDetachedChat(answer)) {
        outcome = "detached";
        // Nothing to render: the reply arrives on the stream, and durably in
        // `chat/history` when the shell sees the turn go terminal. The working
        // row stays up, driven by the open turn rather than by this POST.
        if (chatId) onSendDetached?.(chatId, answer.turnId);
        return true;
      }
      const reply = answer;
      const replies = reply.responses.length
        ? reply.responses.map((r) =>
            makeMessage("company", r.text, {
              channel: r.channel,
              parentId,
              steps: r.steps,
              taskId: r.taskId,
              messageId: r.messageId,
              mentions: r.mentions,
            }),
          )
        : [makeMessage("system", "(no reply)", { parentId })];
      append(target, ...replies);
      // The synchronous response predates mention metadata on some hosts. A
      // reply is already journaled by the time this response arrives, so fetch
      // the authoritative projection and merge its mention DTOs onto the
      // optimistic reply instead of leaving the live row chip-less until a
      // full reload.
      if (chatId && replies.some((reply) => reply.id.startsWith("h"))) {
        void client
          .getChatHistory(chatId, company)
          .then((entries) => {
            // The reply was optimistic; the history fetch that hydrates it lands
            // asynchronously. If the operator switched company, channel or
            // connection in between, this target has been re-homed — merging the
            // old scope's mention DTOs onto an unrelated same-id message would
            // decorate the wrong transcript. Drop the hydration in that case;
            // the next history fetch for the new scope is authoritative.
            const latestScope = scopeRef.current;
            if (
              latestScope &&
              (scopeAtSend.company !== latestScope.company ||
                scopeAtSend.connection !== latestScope.connection ||
                scopeAtSend.client !== latestScope.client)
            ) {
              return;
            }
            const hydrated = fromHistory(entries);
            const byId = new Map(hydrated.map((message) => [message.id, message]));
            setTranscripts((transcripts) => ({
              ...transcripts,
              [target]: (transcripts[target] ?? []).map((message) => {
                const authoritative = byId.get(message.id);
                return authoritative?.mentions
                  ? { ...message, mentions: authoritative.mentions }
                  : message;
              }),
            }));
          })
          .catch(() => {
            /* The next history hydration remains the fallback. */
          });
      }
      onReply?.();
      return true;
    } catch (err) {
      outcome = "failed";
      const latestScope = scopeRef.current;
      if (
        latestScope &&
        (scopeAtSend.company !== latestScope.company ||
          scopeAtSend.connection !== latestScope.connection ||
          scopeAtSend.client !== latestScope.client)
      ) {
        outcome = "stale";
        if (chatId) onSendStale?.(chatId);
        // Unlike the try-block's stale branch above, the request THREW here —
        // whether it journaled before failing is unknown, not "no" (see this
        // function's doc comment), so this is `undefined`, not `false`.
        return undefined;
      }
      // Still said, even when the reply arrives on the stream a moment later:
      // the request did fail, and an operator not told that has no way to know
      // whether their message was taken at all. The two facts are not in
      // competition — this line reports the request, the shell renders whatever
      // the turn goes on to produce.
      const msg = err instanceof ApiError ? err.message : "something went wrong";
      append(target, makeMessage("system", `Couldn't send — ${msg}`, { parentId }));
      // Ambiguous, not a confirmed non-send — see this function's doc comment.
      return undefined;
    } finally {
      // A detached turn ends when its row settles, not when this POST resolves.
      // Calling `onSendEnd` here would clear the live step timeline and take the
      // working row down while the turn is still going.
      //
      // A *failed* POST must not reach `onSendEnd` either, and that one is
      // easier to get wrong because the send really is over. `onSendEnd` tells
      // the shell the reply is on screen, which lets it drop the live frame it
      // was holding — but a throw rendered nothing, and since #983 the turn
      // carries on regardless, so the frame it holds is the only copy of the
      // answer. Routing the throw here is the drop this whole change removes,
      // put back on the one path the feature exists for.
      if (chatId) {
        if (outcome === "resolved") onSendEnd?.(chatId);
        else if (outcome === "failed") onSendFailed?.(chatId);
      }
      setSending(false);
    }
  }

  /**
   * Set or clear the operator's own reaction on a message (issue #364).
   *
   * Optimistic, then reconciled by failure: the chip flips at once because a
   * reaction that waits for a round trip feels broken, and rolls back if the
   * host refuses. It never guesses — a host with no reactions route says so,
   * once, instead of leaving a chip that will be gone on the next reload.
   */
  async function react(messageId: string, emoji: string) {
    const seq = toHostMessageId(messageId);
    if (!seq) return;
    const before = transcripts[active.id] ?? [];
    const on = !before.some(
      (m) => m.id === messageId && m.reactions?.some((r) => r.emoji === emoji && r.mine),
    );
    const apply = (rows: ChatMessage[]) =>
      rows.map((m) =>
        m.id === messageId
          ? { ...m, reactions: toggleReaction(m.reactions, emoji, "you") }
          : m,
      );
    setTranscripts((t) => ({ ...t, [active.id]: apply(t[active.id] ?? []) }));
    try {
      await client.reactToMessage(seq, emoji, on, company);
    } catch (error) {
      setTranscripts((t) => ({ ...t, [active.id]: apply(t[active.id] ?? []) }));
      toast.error(
        error instanceof ApiError && error.status === 404
          ? "This host doesn't keep reactions yet."
          : error instanceof Error
            ? error.message
            : "Couldn't save that reaction.",
      );
    }
  }

  /**
   * Delete the board card a line opened, and stop drawing its chip
   * (issue #984).
   *
   * #442 allowed a turn to open a card from an ordinary message on the
   * grounds that *"a spurious card can be dismissed in one click"*. That click
   * did not exist here: the chip was a bare link to the card's detail screen,
   * so clearing a mis-fired card meant leaving the channel, finding the card
   * on the board, and deleting it there — which is how the board filled up.
   *
   * NOT optimistic, unlike `react` directly above, and the asymmetry is the
   * point. A reaction that rolls back costs nothing; a chip that vanishes
   * while the card survives tells the operator the board is clean when it is
   * not, which is the very confusion this issue is about. So: delete on the
   * host first, clear the chip only on success, and leave it exactly where it
   * was on a refusal.
   *
   * Clears by CARD id, not by the row clicked, and across every channel rather
   * than the active one — see {@link clearTaskCardEverywhere}. Once the card is
   * gone every chip naming it is a link to a 404, and they can sit on different
   * lines and in different channels.
   */
  async function dismissCard(taskId: string) {
    if (dismissingCardId) return;
    setDismissingCardId(taskId);
    try {
      await deleteTask(client, company, taskId);
      clearCardEverywhere(taskId);
      toast.success("Card dismissed.");
    } catch (error) {
      // A 404 is positive proof the card is gone — most likely deleted from the
      // board itself, where nothing tells the open chat surface about it. The
      // chip is then a permanent link to a 404 that no amount of clicking can
      // remove, so this is a success for the operator's purpose: clear it and
      // say so. Only a refusal we cannot interpret leaves the chip in place.
      if (error instanceof ApiError && error.status === 404) {
        clearCardEverywhere(taskId);
        toast.success("That card was already gone — chip cleared.");
      } else {
        toast.error(
          error instanceof Error && error.message ? error.message : "Couldn't dismiss that card.",
        );
      }
    } finally {
      setDismissingCardId(null);
    }
  }

  /** Drop the card from every channel — see {@link clearTaskCardEverywhere}. */
  function clearCardEverywhere(taskId: string) {
    setTranscripts((t) => clearTaskCardEverywhere(t, taskId));
  }

  /**
   * Give a teammate an inbox, or take it away, on the host — keyed by the
   * roster **agent id**, which is the `InboxStore` key the Inbox page reads and
   * the ingest webhook files mail under. Nothing is persisted client-side: if
   * the write fails the switch goes back, so the console never claims an inbox
   * the host doesn't have (issue #173).
   *
   * Starter-roster rows are locally-invented placeholders, not host records, so
   * their ids are not real inbox keys — refuse rather than file mail under one.
   */
  async function toggleMemberInbox(member: TeamMember) {
    if (!fromHost) {
      toast.error("Add this teammate to your company first — an inbox needs a saved teammate.");
      return;
    }
    const next = !member.inboxEnabled;
    const apply = (enabled: boolean) =>
      setMembers((ms) => ms.map((m) => (m.id === member.id ? { ...m, inboxEnabled: enabled } : m)));
    apply(next);
    try {
      await setInboxEnabled(client, company, member.id, next);
    } catch (error) {
      apply(!next);
      toast.error(
        error instanceof ApiError && error.status === 404
          ? "This host doesn't offer teammate inboxes yet."
          : error instanceof Error
            ? error.message
            : "Couldn't change the inbox.",
      );
    }
  }

  /**
   * Persist a new teammate through the host (issue #360's Team-page add path),
   * falling back to a local-only add for a host without the write plane yet —
   * the same 404 fallback `boot` uses for the roster read itself.
   */
  async function addMember(fields: NewMemberFields) {
    let created: TeamMemberDto | null = null;
    try {
      created = await client.addTeamMember(
        { name: fields.name, role: fields.role, description: fields.description || undefined },
        company,
      );
    } catch (error) {
      if (error instanceof ApiError && error.status === 404) {
        // No team write plane on this host — keep the add local-only.
        setMembers((m) => [...m, newMember(fields)]);
      } else {
        reportAddMember(addMemberFailure(error));
        return;
      }
    }
    let outcome: AddMemberOutcome;
    if (created) {
      const member = fromDto(created);
      setMembers((m) => [...m, member]);
      // The host directory is re-read so the new teammate can be @-mentioned
      // from the picker immediately, rather than after the next reload.
      void reloadDirectory();
      // A successful host add proves the write plane exists, even for a
      // company that opened on the starter roster (fromHost still false from
      // `boot`) — flip it so this and later actions (inbox, budget) target
      // the host instead of refusing on a now-stale local-only guard.
      setFromHost(true);
      outcome = { kind: "added", name: fields.name };
      // A host-backed add has a real agent id, so the inbox request can go
      // straight through rather than waiting for a second save.
      if (fields.inbox) {
        try {
          await setInboxEnabled(client, company, member.id, true);
          setMembers((ms) => ms.map((m) => (m.id === member.id ? { ...m, inboxEnabled: true } : m)));
        } catch {
          outcome = {
            kind: "partial",
            name: fields.name,
            missed: "their inbox couldn't be switched on.",
            fix: "Add it from the teammate's actions menu.",
          };
        }
      }
    } else {
      // A locally-added teammate has no host record yet, so there is no agent
      // id to hang an inbox off — say so rather than silently dropping it.
      outcome = {
        kind: "console-only",
        name: fields.name,
        note: fields.inbox
          ? "Save them on the host before giving them an inbox."
          : undefined,
      };
    }
    setAddOpen(false);
    reportAddMember(outcome);
  }

  /**
   * Drop a teammate from the roster through the host when it has a record of
   * them. A blueprint teammate is removable too — the host records a tombstone
   * rather than rewriting `company.toml` — and the only refusal left is the
   * company's last teammate (409). A starter-roster row has no host record at
   * all, so it falls back to a local-only removal.
   */
  async function removeMember(member: TeamMember) {
    if (!fromHost) {
      setMembers((ms) => ms.filter((m) => m.id !== member.id));
      return;
    }
    try {
      await client.removeTeamMember(member.id, company);
      setMembers((ms) => ms.filter((m) => m.id !== member.id));
      // The removed teammate leaves the picker now, not on the next reload.
      void reloadDirectory();
    } catch (error) {
      if (error instanceof ApiError && error.status === 409) {
        // The only 409 this route still answers: a company must keep at
        // least one teammate. The host's own message says which teammate and
        // what to do about it, so it is shown rather than restated.
        toast.error(
          error.message || "You can't remove your company's last teammate.",
        );
      } else {
        toast.error(error instanceof Error ? error.message : "Couldn't remove teammate.");
      }
    }
  }

  function selectChannel(id: string) {
    onNavigate(id);
    setMobilePane("chat");
  }

  const parent = openThreadId ? messages.find((m) => m.id === openThreadId) : undefined;
  const threadReplies = parent ? messages.filter((m) => m.parentId === parent.id) : [];

  return (
    <div className="flex min-h-0 flex-1">
      {/* The channel rail and the chat pane share the viewport with the app
          sidebar. That sidebar is on from `md` (≥768), so a rail that also came
          in at `md` gave two rails plus content a ~290px pane from 768–1023px —
          Send fell off the right edge with no scroll to reach it (issue #1383).
          The rail now waits for `lg` (≥1024); from 768–1023 the pane runs
          single-column and the "Show channels" toggle in the header (also
          `lg:hidden`) swaps to the rail, mirroring the sub-`md` mobile flow. */}
      <ChannelRail
        sections={sections}
        activeId={channel.id}
        unread={unread ?? {}}
        mentions={mentions}
        onSelect={selectChannel}
        openSections={railOpenSections}
        onToggleSection={toggleRailSection}
        directMessages={directMessageChannels(members)}
        onStartDirectMessage={selectChannel}
        className={cn("lg:hidden", mobilePane === "rail" ? "flex" : "hidden")}
      />
      <ChannelRail
        sections={sections}
        activeId={channel.id}
        unread={unread ?? {}}
        mentions={mentions}
        onSelect={selectChannel}
        openSections={railOpenSections}
        onToggleSection={toggleRailSection}
        directMessages={directMessageChannels(members)}
        onStartDirectMessage={selectChannel}
        collapsed={channelsCollapsed}
        onExpand={toggleChannels}
        className="hidden lg:flex"
      />

      <div
        className={cn(
          "min-w-0 flex-1 flex-col",
          mobilePane === "chat" ? "flex" : "hidden lg:flex",
        )}
      >
        <ChatHeader
          channel={channel}
          memberCount={headerCount}
          membersOpen={membersOpen}
          onToggleMembers={() => setMembersOpen((o) => !o)}
          onOpenRail={() => setMobilePane("rail")}
          channelsCollapsed={channelsCollapsed}
          onToggleChannels={toggleChannels}
          channelsToggleRef={channelsToggleRef}
        />

        <div className="flex min-h-0 flex-1">
          <div className="flex min-w-0 flex-1 flex-col">
            {unknownChannel && (
              <p
                role="status"
                className="flex shrink-0 items-center gap-1.5 border-b bg-muted/50 px-3 py-1.5 text-xs text-muted-foreground"
              >
                <TriangleAlert className="size-3.5 shrink-0" aria-hidden />
                <span className="min-w-0 truncate">
                  <span className="font-medium text-foreground">#{unknownChannel}</span> isn&apos;t a
                  channel here — showing {channelTitle(active)} instead.
                </span>
              </p>
            )}
            <MessageTimeline
              channel={channel}
              items={items}
              historyPending={historyPending}
              openThreadId={openThreadId}
              // An open turn keeps the row up after the POST has resolved, and
              // puts it back on a console that reloaded mid-turn (#983).
              typing={(sending || !!openTurn) && !openThreadId}
              queued={!!openTurn?.queued}
              liveSteps={openThreadId ? undefined : liveSteps}
              onOpenThread={setOpenThreadId}
              onReact={react}
              onDismissCard={(taskId) => void dismissCard(taskId)}
              dismissingCardId={dismissingCardId}
              resolveAttachmentUrl={resolveAttachmentUrl}
              onStartBrief={() =>
                setComposerPrefill((current) => ({
                  text: FIRST_TEAM_BRIEF,
                  revision: (current?.revision ?? 0) + 1,
                }))
              }
              onAddPeople={() => setMembersOpen(true)}
              now={now}
              askerNames={askerNames}
              decidingApprovals={decidingApprovals}
              failedApprovals={failedApprovals}
              onDecideApproval={onDecideApproval}
            />
            {consoleOnlyMember && (
              <p
                role="status"
                className="flex shrink-0 items-center gap-1.5 border-t bg-muted/50 px-3 py-1.5 text-xs text-muted-foreground"
              >
                <TriangleAlert className="size-3.5 shrink-0" aria-hidden />
                <span className="min-w-0">
                  <span className="font-medium text-foreground">{consoleOnlyMember}</span> only
                  exists in this console — the company has no such teammate, so nobody answers
                  here. The transcript is still saved and survives a reload.
                </span>
              </p>
            )}
            <TypingLine names={resolveTypingNames?.(active.id) ?? []} />
            <MessageComposer
              placeholder={`Message ${channelTitle(channel)}`}
              disabled={sending}
              prefill={composerPrefill ?? undefined}
              // Not voided (unlike the thread composer below): the composer
              // awaits this to know whether an attachment it carried actually
              // journaled, so it can clean up one that did not (codex review
              // finding on #1682) — see `deleteAttachment` and `send`'s doc.
              onSend={(text, intent, attachments, mentions) =>
                send(text, intent, undefined, attachments, mentions)
              }
              // Issue #1682: only the channel/DM composer attaches — the paperclip
              // is present exactly because this prop is.
              uploadAttachment={uploadAttachment}
              // Cleans up a staged upload that never got sent (codex review
              // finding on #1682) — see `deleteAttachment`.
              deleteAttachment={deleteAttachment}
              // Every keystroke asks; the hook throttles to one ping per
              // channel per few seconds and skips entirely while the event
              // stream is down.
              onTyping={() => onTyping?.(active.id)}
              // Channel *and* DM composers offer "just chatting" / "do it once" /
              // "build me the workflow" (issues #580, #845, #1152) — see
              // `offersDeliverableChoice`, which owns the rule and is unchanged:
              // the new position inherits the same channel+DM gating. Only the
              // thread and copilot composers below go without.
              deliverableChoice={offersDeliverableChoice(active.kind)}
              mentionables={mentionables}
              channelMemberIds={inChannel?.map((m) => m.id)}
            />
          </div>

          {parent && (
            <ThreadPanel
              channel={channel}
              members={members}
              youAvatar={youAvatar}
              parent={parent}
              replies={threadReplies}
              sending={sending}
              mentionables={mentionables}
              channelMemberIds={inChannel?.map((m) => m.id)}
              resolveAttachmentUrl={resolveAttachmentUrl}
              onSend={(text, _intent, _attachments, mentions) =>
                void send(text, undefined, parent.id, undefined, mentions)
              }
              onClose={() => setOpenThreadId(null)}
              typingNames={resolveTypingNames?.(active.id, parent.id) ?? []}
              onTyping={() => onTyping?.(active.id, parent.id)}
            />
          )}

          {membersOpen && (
            <MembersPane
              channelMembers={inChannel}
              others={outsideChannel}
              people={companyPeople}
              presence={presence}
              leadId={active.kind === "channel" ? active.memberIds?.[0] : undefined}
              loading={loadingTeam}
              fromHost={fromHost}
              onToggleInbox={(m) => void toggleMemberInbox(m)}
              onRemove={(id) => {
                const member = members.find((m) => m.id === id);
                if (member) void removeMember(member);
              }}
              onAdd={() => setAddOpen(true)}
              onMessage={(m) => selectChannel(dmChannelId(m))}
              /**
               * The way from this channel to the desk it is (issue #485).
               *
               * Only for a host-backed desk channel. A DM is not a desk, and a
               * fallback desk (`lib/desks.ts`) carries no `memberIds` because
               * the host has no desks surface at all — the chart would have
               * nothing to open. Both simply get no link rather than one that
               * lands nowhere.
               *
               * A desk's channel id **is** its desk id (`deskFromDto`), so
               * there is no mapping to keep in step. Written to the hash rather
               * than routed through a callback, as `ArtifactsTab`'s "Open in
               * workspace" does: this is a cross-view address, and the shell
               * only hands chat a chat-scoped navigate.
               */
              onManageDesk={
                active.kind === "channel" && active.memberIds
                  ? () => {
                      window.location.hash = `/company/${active.id}`;
                    }
                  : undefined
              }
              canEditBudget={isAdmin && fromHost}
              onEditBudget={setBudgetFor}
              onRemoveCap={(m) => void applyBudget(m, null)}
              onResetBudget={(m) => void resetBudget(m)}
              setByLabel={(m) => (m.budgetSetBy ? whoSet(m.budgetSetBy) : undefined)}
            />
          )}
        </div>
      </div>

      <AddMemberDialog open={addOpen} onOpenChange={setAddOpen} onAdd={(fields) => void addMember(fields)} />
      <BudgetDialog
        member={budgetFor}
        onOpenChange={(open) => {
          if (!open) setBudgetFor(null);
        }}
        onSave={(cap) => {
          const target = budgetFor;
          setBudgetFor(null);
          if (target) void applyBudget(target, cap);
        }}
      />
    </div>
  );
}

/**
 * The pane while `/desks` is still out.
 *
 * Shaped like the workspace it is about to become — a header bar and message
 * rows — so the real channel does not arrive as a jump. It exists to say "not
 * yet", which is the one thing the old blank pane could not distinguish from
 * "never".
 */
function LoadingPane() {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-13 shrink-0 items-center gap-2 border-b px-3">
        <Skeleton className="size-4 rounded" />
        <Skeleton className="h-4 w-32 rounded" />
      </div>
      <div className="flex-1 space-y-3 p-4">
        {Array.from({ length: 5 }).map((_, i) => (
          <Skeleton key={i} className="h-12 rounded-lg" />
        ))}
      </div>
      <span className="sr-only">Loading channels…</span>
    </div>
  );
}

/** A whole-pane state: a headline, a sentence, and at most one thing to do. */
function EmptyPane({
  title,
  body,
  action,
  after,
}: {
  title: string;
  body: string;
  action?: { label: string; onClick: () => void };
  /** Rendered alongside — a dialog the action opens, which needs to mount. */
  after?: ReactNode;
}) {
  return (
    <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-3 p-8 text-center">
      <div className="max-w-sm space-y-1.5">
        <h2 className="text-base font-semibold tracking-tight">{title}</h2>
        <p className="text-sm text-muted-foreground">{body}</p>
      </div>
      {action && (
        <Button variant="outline" size="sm" onClick={action.onClick}>
          {action.label}
        </Button>
      )}
      {after}
    </div>
  );
}
