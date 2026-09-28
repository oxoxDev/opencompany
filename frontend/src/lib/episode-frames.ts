/**
 * The live half of an episode: a bounded fold over the SSE frames the host
 * emits while a desk answers as a room.
 *
 * # What this holds and what it does not
 *
 * The transcript is the durable record — every committed utterance is an
 * `agent_reply` row carrying `episode`, and `lib/episodes.ts` rebuilds the
 * rounds from those rows alone after a reload. What the rows cannot say is
 * **what is happening right now**: which seats a round opened with, which of
 * them is still working, which one timed out and said nothing. Only the frames
 * say that, so this reducer keeps exactly that and nothing the rows already
 * carry in full.
 *
 * Pure, so every rule below is unit-testable without a browser: a `turn_started`
 * for a round the console never saw open mints the round; a `turn_settled` with
 * no `episodeId` is not an episode's business and returns the same object; a
 * `round_committed` marks every seat it names committed even if that seat's
 * settle frame was lost.
 *
 * # Bounded
 *
 * A console left open on a busy company sees frames for every desk, forever.
 * {@link EPISODE_FRAME_CAP} episodes are kept, completed ones evicted first,
 * then the oldest — the band for an episode that scrolled out of the cap is
 * rebuilt from the transcript on the next hydration, so eviction costs a live
 * lane, never a wrong one.
 */

import type { RoutingPlanDto, RoutingRouter, UtteranceKind } from "@/api/types";
import type { EpisodeFrame, TurnBracketFrame } from "@/hooks/use-events";

/** What one seat of a round is doing, as the frames have said so far. */
export type SeatStatus =
  | "waiting"
  | "working"
  | "committed"
  | "failed"
  | "timed_out"
  | "no_utterance";

/** One seat's live state inside a round. */
export interface SeatState {
  agentId: string;
  status: SeatStatus;
  startedAtMillis?: number;
  settledAtMillis?: number;
  /** The utterance the driver committed for this seat, once it did. */
  utterance?: {
    kind: UtteranceKind;
    sequence: number;
    messageSeq?: number;
    to?: string[];
  };
}

/** One round of an episode: the seats it opened with and where each one is. */
export interface RoundState {
  revision: number;
  /** In the order the host named them — the lane order. */
  agentIds: string[];
  seats: Record<string, SeatState>;
  status: "open" | "committed";
  startedAtMillis: number;
  committedAtMillis?: number;
}

/** A broadcast the driver routed onward, and to whom. */
export interface BroadcastRecord {
  revision: number;
  agentId: string;
  messageSeq: number;
  plan: RoutingPlanDto;
  router: RoutingRouter;
  probabilities?: Record<string, number>;
  atMillis: number;
}

/** A desk DM the driver delivered. */
export interface DmRecord {
  from: string;
  to: string[];
  messageSeq: number;
  atMillis: number;
}

/** A private exchange between two seats of this desk.
 *
 * `endedAtMillis` is what turns the live indicator off. A conversation that
 * ran out of turns ends `forced`, without an answer, and an indicator that
 * only watched for an answer would hang on exactly that case. */
export interface ConversationRecord {
  /** The `ask` row it is rooted at, and the key it is folded by. */
  root: number;
  asker: string;
  askee: string;
  /** The channel the exchange itself is in. */
  conversationId: string;
  openedAtMillis: number;
  endedAtMillis?: number;
  /** Ended without an answer. */
  forced?: boolean;
  /**
   * The desk row this exchange hangs off: the ask's own parent.
   *
   * Derived here rather than carried on the frame, because the console has
   * already seen the ask -- it is an ordinary reply row in the pair channel,
   * and the stream is company-wide -- so the parent it names is in hand by
   * the time the exchange opens. The host anchors a running exchange the same
   * way, so live and reloaded agree on where it sits.
   */
  anchorId?: number;
  /**
   * The exchange so far, as it lands.
   *
   * The rows are in the pair channel and the desk never shows them, so
   * without this the widget has nothing to say until a reload fetches the
   * host's fold -- which is exactly how it behaved: silent for the whole
   * exchange, complete only once it was over.
   */
  lines: { authorId: string; text: string; outbound: boolean }[];
}

/** A crossing raised from inside this episode. */
export interface EpisodeReferral {
  toDesk: string;
  target: string;
  asker: string;
  direct: boolean;
  returning: boolean;
  sequence: number;
  toEpisodeId?: string;
  atMillis: number;
}

/** A seat waiting on an operator decision. */
export interface ParkedSeat {
  agentId: string;
  approvalIds: string[];
  /** The conversation root the seat was in; absent on the desk. */
  thread?: number;
  atMillis: number;
}

/** Everything the frames have said about one episode. */
export interface EpisodeState {
  id: string;
  chatId: string;
  openedBySeq?: number;
  parentId?: string;
  participants: string[];
  plan?: RoutingPlanDto;
  /** Keyed by revision. */
  rounds: Record<number, RoundState>;
  status: "open" | "completed";
  openedAtMillis?: number;
  completedAtMillis?: number;
  completedBy?: string;
  /** Widened: a word from a newer host is still a reason. */
  reason?: string;
  /** How many rounds the host counted at completion. */
  roundCount?: number;
  summarySeq?: number;
  broadcasts: BroadcastRecord[];
  dms: DmRecord[];
  /** Keyed by the ask row that roots each one, so the concluding frame
   *  finds the record the opening frame made. */
  conversations: Record<number, ConversationRecord>;
  /**
   * Pair-channel rows seen before the exchange that owns them opened.
   *
   * The `ask` is journaled before `conversation_opened`, so its row arrives
   * first and there is nothing yet to file it under. Held by sequence until
   * an exchange claims it, and dropped with the episode.
   */
  pending: Record<number, { chatId: string; authorId: string; text: string; parentId?: number }>;
  referrals: EpisodeReferral[];
  /** Seats parked on the operator, by agent id, until they resume or the episode ends. */
  parked: Record<string, ParkedSeat>;
  /** The newest frame sequence folded, for eviction order and tests. */
  lastSeq: number;
}

/** The fold's whole state: episodes by id, plus their arrival order. */
export interface EpisodeFrames {
  byId: Record<string, EpisodeState>;
  /** Oldest first. */
  order: string[];
}

/** The state before any frame — the same object every time, so React sees no change. */
export const EMPTY_EPISODE_FRAMES: EpisodeFrames = Object.freeze({
  byId: {},
  order: [],
}) as EpisodeFrames;

/** How many episodes the fold keeps before evicting. */
export const EPISODE_FRAME_CAP = 48;

/** How `turn_settled.outcome` maps onto a seat's status. Unknown words read as committed. */
function seatStatusFor(outcome: string | undefined): SeatStatus {
  switch (outcome) {
    case "failed":
      return "failed";
    case "timed_out":
      return "timed_out";
    case "no_utterance":
      return "no_utterance";
    default:
      return "committed";
  }
}

function mintEpisode(id: string, chatId: string, seq: number): EpisodeState {
  return {
    id,
    chatId,
    participants: [],
    rounds: {},
    status: "open",
    broadcasts: [],
    dms: [],
    conversations: {},
    pending: {},
    referrals: [],
    parked: {},
    lastSeq: seq,
  };
}

function mintRound(revision: number, agentIds: string[], atMillis: number): RoundState {
  const seats: Record<string, SeatState> = {};
  for (const agentId of agentIds) seats[agentId] = { agentId, status: "waiting" };
  return { revision, agentIds, seats, status: "open", startedAtMillis: atMillis };
}

/** A copy of the round with `agentId` present as a seat, minted waiting if new. */
function withSeat(round: RoundState, agentId: string): RoundState {
  if (round.seats[agentId]) return round;
  return {
    ...round,
    agentIds: [...round.agentIds, agentId],
    seats: { ...round.seats, [agentId]: { agentId, status: "waiting" } },
  };
}

/**
 * Folds one frame into the state, returning the same object when the frame
 * says nothing about an episode.
 */
export function reduceEpisodeFrame(
  state: EpisodeFrames,
  frame: EpisodeFrame | TurnBracketFrame,
): EpisodeFrames {
  const episodeId = "episodeId" in frame ? frame.episodeId : undefined;
  if (!episodeId) return state;

  const chatId = "chatId" in frame && frame.chatId ? frame.chatId : "";
  const held = state.byId[episodeId];
  let episode: EpisodeState = held
    ? { ...held, lastSeq: Math.max(held.lastSeq, frame.seq) }
    : mintEpisode(episodeId, chatId, frame.seq);
  if (!episode.chatId && chatId) episode = { ...episode, chatId };

  switch (frame.type) {
    case "episode_opened":
      episode = {
        ...episode,
        chatId: frame.chatId,
        openedBySeq: frame.openedBySeq,
        parentId: frame.parentId,
        participants: frame.participants,
        plan: frame.plan,
        openedAtMillis: frame.atMillis,
      };
      break;
    case "round_started": {
      const existing = episode.rounds[frame.revision];
      // Merge rather than replace: a seat's `turn_started` can arrive before
      // the round's own frame when the host emits them from different tasks,
      // and its working state must survive the round frame landing.
      const round = existing
        ? {
            ...existing,
            agentIds: [
              ...frame.agentIds,
              ...existing.agentIds.filter((id) => !frame.agentIds.includes(id)),
            ],
            seats: Object.fromEntries(
              [...frame.agentIds, ...existing.agentIds].map((id) => [
                id,
                existing.seats[id] ?? { agentId: id, status: "waiting" },
              ]),
            ),
            startedAtMillis: Math.min(existing.startedAtMillis, frame.atMillis),
          }
        : mintRound(frame.revision, frame.agentIds, frame.atMillis);
      episode = { ...episode, rounds: { ...episode.rounds, [frame.revision]: round } };
      break;
    }
    case "turn_started":
    case "turn_settled": {
      if (frame.roundRevision === undefined || !frame.agentId) return state;
      const base =
        episode.rounds[frame.roundRevision] ??
        mintRound(frame.roundRevision, [], frame.atMillis);
      const round = withSeat(base, frame.agentId);
      const seat = round.seats[frame.agentId];
      const next: SeatState =
        frame.type === "turn_started"
          ? { ...seat, status: "working", startedAtMillis: frame.atMillis }
          : // A settle never demotes a seat the commit already spoke for:
            // `round_committed` is the driver's word, and it can land first.
            seat.status === "committed"
            ? { ...seat, settledAtMillis: frame.atMillis }
            : {
                ...seat,
                status: seatStatusFor(frame.outcome),
                settledAtMillis: frame.atMillis,
              };
      episode = {
        ...episode,
        rounds: {
          ...episode.rounds,
          [frame.roundRevision]: { ...round, seats: { ...round.seats, [frame.agentId]: next } },
        },
      };
      break;
    }
    case "round_committed": {
      let round =
        episode.rounds[frame.revision] ?? mintRound(frame.revision, [], frame.atMillis);
      for (const utterance of frame.utterances) {
        round = withSeat(round, utterance.agentId);
        const seat = round.seats[utterance.agentId];
        round = {
          ...round,
          seats: {
            ...round.seats,
            [utterance.agentId]: {
              ...seat,
              status: "committed",
              settledAtMillis: seat.settledAtMillis ?? frame.atMillis,
              utterance: {
                kind: utterance.kind,
                sequence: utterance.sequence,
                messageSeq: utterance.messageSeq,
                to: utterance.to,
              },
            },
          },
        };
      }
      // A seat still shown working after the commit said nothing for it
      // produced no utterance — the driver moved on without it.
      const seats = { ...round.seats };
      for (const seat of Object.values(seats)) {
        if (seat.status === "working" || seat.status === "waiting") {
          seats[seat.agentId] = { ...seat, status: "no_utterance", settledAtMillis: frame.atMillis };
        }
      }
      round = { ...round, seats, status: "committed", committedAtMillis: frame.atMillis };
      episode = { ...episode, rounds: { ...episode.rounds, [frame.revision]: round } };
      break;
    }
    case "broadcast_routed":
      episode = {
        ...episode,
        broadcasts: [
          ...episode.broadcasts,
          {
            revision: frame.revision,
            agentId: frame.agentId,
            messageSeq: frame.messageSeq,
            plan: frame.plan,
            router: frame.router,
            probabilities: frame.probabilities,
            atMillis: frame.atMillis,
          },
        ],
      };
      break;
    case "dm_delivered":
      episode = {
        ...episode,
        dms: [
          ...episode.dms,
          { from: frame.from, to: frame.to, messageSeq: frame.messageSeq, atMillis: frame.atMillis },
        ],
      };
      break;
    case "conversation_opened": {
      // The ask is already in hand: it was journaled to the pair channel
      // before this reference, so it arrived as an ordinary reply row and is
      // waiting in `pending`. It is both the exchange's first line and the
      // thing that names the desk row to hang it on.
      const ask = episode.pending[frame.root];
      episode = {
        ...episode,
        conversations: {
          ...episode.conversations,
          [frame.root]: {
            root: frame.root,
            asker: frame.asker,
            askee: frame.askee,
            conversationId: frame.conversationId,
            openedAtMillis: frame.atMillis,
            anchorId: ask?.parentId,
            lines: ask ? [{ authorId: ask.authorId, text: ask.text, outbound: true }] : [],
          },
        },
      };
      break;
    }
    case "agent_reply": {
      // Only the pair channels matter here; a desk row is the transcript's
      // own business and the fold has never read one.
      if (!frame.chatId.startsWith("dm:")) break;
      // The conclusion is threaded under the ask so a seat's thread read
      // reaches it; as a line it would be the askee's last one said twice.
      if (frame.utteranceKind === "dm") break;
      const parentId = frame.parentId === undefined ? undefined : Number(frame.parentId);
      const root = Object.keys(episode.conversations)
        .map(Number)
        .find((candidate) => candidate === parentId);
      if (root === undefined) {
        // Not claimed yet -- most often the `ask` itself, which lands before
        // the reference that opens its exchange.
        episode = {
          ...episode,
          pending: {
            ...episode.pending,
            [frame.seq]: {
              chatId: frame.chatId,
              authorId: frame.agentId,
              text: frame.text,
              parentId,
            },
          },
        };
        break;
      }
      const held = episode.conversations[root];
      episode = {
        ...episode,
        conversations: {
          ...episode.conversations,
          [root]: {
            ...held,
            lines: [
              ...held.lines,
              {
                authorId: frame.agentId,
                text: frame.text,
                outbound: frame.agentId === held.asker,
              },
            ],
          },
        },
      };
      break;
    }
    case "conversation_concluded": {
      // Merge rather than replace: the concluding frame carries the pair and
      // the channel too, so a fold that started mid-episode and never saw
      // the opening still ends with a usable record.
      const opened = episode.conversations[frame.root];
      episode = {
        ...episode,
        conversations: {
          ...episode.conversations,
          [frame.root]: {
            ...(opened ?? {
              root: frame.root,
              asker: frame.asker,
              askee: frame.askee,
              conversationId: frame.conversationId,
              openedAtMillis: frame.atMillis,
            }),
            endedAtMillis: frame.atMillis,
            forced: frame.forced,
          },
        },
      };
      break;
    }
    case "episode_completed":
      episode = {
        ...episode,
        status: "completed",
        completedAtMillis: frame.atMillis,
        completedBy: frame.completedBy,
        reason: frame.reason,
        roundCount: frame.rounds,
        summarySeq: frame.summarySeq,
        parked: {},
        // **Settle whatever was still open.** The wave a seat completes the
        // episode from never gets a `round_committed` of its own — the
        // episode ends under it — so without this its seats stay `working`
        // and its status stays `open` for good. The band then reads
        // "running together" beside a completion marker, and its lanes spin
        // forever: a finished episode that looks like a live one, which is
        // the one thing a live indicator must never say.
        //
        // `no_utterance` rather than a status of its own: these seats
        // committed nothing, which is exactly what the word means. It makes
        // no claim about why.
        rounds: Object.fromEntries(
          Object.entries(episode.rounds).map(([revision, round]) => [
            revision,
            round.status === "open"
              ? {
                  ...round,
                  status: "committed" as const,
                  seats: Object.fromEntries(
                    Object.entries(round.seats).map(([id, seat]) => [
                      id,
                      seat.status === "working" || seat.status === "waiting"
                        ? { ...seat, status: "no_utterance" as const, settledAtMillis: seat.settledAtMillis ?? frame.atMillis }
                        : seat,
                    ]),
                  ),
                }
              : round,
          ]),
        ),
      };
      break;
    case "episode_seat_parked":
      if (episode.status === "completed") return state;
      episode = {
        ...episode,
        parked: {
          ...episode.parked,
          [frame.seat]: {
            agentId: frame.seat,
            approvalIds: [
              ...(episode.parked[frame.seat]?.approvalIds ?? []).filter(
                (id) => !frame.approvalIds.includes(id),
              ),
              ...frame.approvalIds,
            ],
            thread: frame.thread,
            atMillis: frame.atMillis,
          },
        },
      };
      break;
    case "episode_seat_resumed": {
      if (!episode.parked[frame.seat]) return state;
      const parked = { ...episode.parked };
      delete parked[frame.seat];
      episode = { ...episode, parked };
      break;
    }
    case "referral":
      episode = {
        ...episode,
        referrals: [
          ...episode.referrals,
          {
            toDesk: frame.toDesk,
            target: frame.target,
            asker: frame.asker,
            direct: frame.direct,
            returning: frame.returning,
            sequence: frame.sequence,
            toEpisodeId: frame.toEpisodeId,
            atMillis: frame.atMillis,
          },
        ],
      };
      break;
    default:
      return state;
  }

  const order = held ? state.order : [...state.order, episodeId];
  return evict({ byId: { ...state.byId, [episodeId]: episode }, order });
}

/** Keeps the fold under {@link EPISODE_FRAME_CAP}: completed first, then oldest. */
function evict(state: EpisodeFrames): EpisodeFrames {
  if (state.order.length <= EPISODE_FRAME_CAP) return state;
  const order = [...state.order];
  const byId = { ...state.byId };
  while (order.length > EPISODE_FRAME_CAP) {
    const completed = order.findIndex((id) => byId[id]?.status === "completed");
    const [gone] = order.splice(completed === -1 ? 0 : completed, 1);
    delete byId[gone];
  }
  return { byId, order };
}

/** The episodes of one desk, oldest first. */
export function episodesOf(state: EpisodeFrames, chatId: string): EpisodeState[] {
  return state.order
    .map((id) => state.byId[id])
    .filter((episode): episode is EpisodeState => !!episode && episode.chatId === chatId);
}

/** Every round of every episode, for a surface that wants them all. */
export function allRounds(state: EpisodeFrames): { episode: EpisodeState; round: RoundState }[] {
  const out: { episode: EpisodeState; round: RoundState }[] = [];
  for (const id of state.order) {
    const episode = state.byId[id];
    if (!episode) continue;
    for (const round of Object.values(episode.rounds)) out.push({ episode, round });
  }
  return out;
}
