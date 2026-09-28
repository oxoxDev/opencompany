/**
 * What the company's coordination actually looked like: how many seats ran
 * at once, who spoke to whom, and how each episode ended.
 *
 * Two folds, both pure:
 *
 * - {@link reduceTurnBracket} keeps a ledger of the chat turn brackets
 *   (`turn_started` / `turn_settled`, issue #983), episode or not. Concurrency
 *   is a property of turns, and a DM turn overlapping a round is still two
 *   models thinking at once — so this reads the bracket and not the band.
 * - {@link coordinationObservations} turns the episode fold into the edges the
 *   comms graph draws: a broadcast is an agent reaching the seats the router
 *   picked, a desk DM is an agent reaching a teammate, a referral is an agent
 *   reaching another desk.
 *
 * `scripts/measure-coordination.mjs` computes the same numbers over the raw
 * SSE feed for a run nobody is watching; the two are kept in step by hand and
 * by the thresholds that script asserts.
 */

import type { TurnBracketFrame } from "@/hooks/use-events";
import type { EpisodeFrames } from "@/lib/episode-frames";
import type { CommsObservation } from "@/views/comms/model";

/** One turn the ledger saw open. */
export interface OpenTurn {
  key: string;
  agentId?: string;
  episodeId?: string;
  roundRevision?: number;
  startedAtMillis: number;
}

/** A settled turn, kept for the pairs and the peak. */
export interface ClosedTurn extends OpenTurn {
  settledAtMillis: number;
}

/** The bracket ledger. */
export interface TurnLedger {
  open: OpenTurn[];
  /** Newest last, bounded by {@link TURN_LEDGER_CAP}. */
  closed: ClosedTurn[];
  /** The most turns open at once, ever. */
  peak: number;
  /** How many times one agent's turn started while its own was still open —
   *  the number the runtime promises is zero. */
  sameAgentOverlaps: number;
}

export const EMPTY_TURN_LEDGER: TurnLedger = Object.freeze({
  open: [],
  closed: [],
  peak: 0,
  sameAgentOverlaps: 0,
}) as TurnLedger;

/** How many settled turns the ledger keeps. */
export const TURN_LEDGER_CAP = 256;

function keyOf(frame: TurnBracketFrame): string {
  return frame.turnId ?? `${frame.agentId ?? "?"}:${frame.chatId ?? "?"}`;
}

/** Folds one bracket frame. Same object back for a frame that changes nothing. */
export function reduceTurnBracket(ledger: TurnLedger, frame: TurnBracketFrame): TurnLedger {
  if (frame.type === "turn_started") {
    const key = keyOf(frame);
    const sameAgent =
      frame.agentId !== undefined && ledger.open.some((turn) => turn.agentId === frame.agentId);
    const open = [
      ...ledger.open,
      {
        key,
        agentId: frame.agentId,
        episodeId: frame.episodeId,
        roundRevision: frame.roundRevision,
        startedAtMillis: frame.atMillis,
      },
    ];
    return {
      open,
      closed: ledger.closed,
      peak: Math.max(ledger.peak, open.length),
      sameAgentOverlaps: ledger.sameAgentOverlaps + (sameAgent ? 1 : 0),
    };
  }
  // A settle matches its start by turn id, else by agent (oldest first), else
  // it is a settle for a turn this console never saw start — a reconnect —
  // and there is nothing to close.
  const key = keyOf(frame);
  let index = ledger.open.findIndex((turn) => turn.key === key);
  if (index === -1 && frame.agentId !== undefined) {
    index = ledger.open.findIndex((turn) => turn.agentId === frame.agentId);
  }
  if (index === -1) return ledger;
  const turn = ledger.open[index];
  const open = ledger.open.filter((_, i) => i !== index);
  const closed = [...ledger.closed, { ...turn, settledAtMillis: frame.atMillis }].slice(
    -TURN_LEDGER_CAP,
  );
  return { ...ledger, open, closed };
}

/** The agents whose turns are open right now. */
export function workingAgents(ledger: TurnLedger): string[] {
  const out: string[] = [];
  for (const turn of ledger.open) {
    if (turn.agentId && !out.includes(turn.agentId)) out.push(turn.agentId);
  }
  return out;
}

/** The kind of contact a coordination edge records. */
export type SpokeVia = "broadcast" | "dm" | "referral";

/**
 * The comms-graph observations the episode fold implies.
 *
 * Every edge is agent → agent or agent → desk, and every one of them was
 * watched happen, so they are all history edges (`spoke`), never structure.
 * A broadcast whose plan named nobody but its author draws nothing: a room
 * talking to itself is not a pair.
 */
export function coordinationObservations(
  frames: EpisodeFrames,
  ledger?: TurnLedger,
): CommsObservation[] {
  const out: CommsObservation[] = [];
  for (const id of frames.order) {
    const episode = frames.byId[id];
    if (!episode) continue;
    for (const broadcast of episode.broadcasts) {
      for (const to of planTargets(broadcast.plan)) {
        if (to === broadcast.agentId) continue;
        out.push({ kind: "spoke", from: broadcast.agentId, to, via: "broadcast", atMillis: broadcast.atMillis });
      }
    }
    for (const dm of episode.dms) {
      for (const to of dm.to) {
        if (to === dm.from) continue;
        out.push({ kind: "spoke", from: dm.from, to, via: "dm", atMillis: dm.atMillis });
      }
    }
    for (const referral of episode.referrals) {
      if (referral.returning) continue;
      out.push({
        kind: "spoke",
        from: referral.asker,
        to: referral.direct ? referral.target : referral.toDesk,
        via: "referral",
        atMillis: referral.atMillis,
      });
    }
  }
  if (ledger) {
    for (const agentId of workingAgents(ledger)) out.push({ kind: "speaking", agentId });
  }
  return out;
}

/** The seats a routing plan hands a broadcast to. */
export function planTargets(plan: { kind: string; primaryId?: string; invitedIds?: string[] }): string[] {
  const out: string[] = [];
  if (plan.primaryId) out.push(plan.primaryId);
  for (const id of plan.invitedIds ?? []) if (!out.includes(id)) out.push(id);
  return out;
}

/** The numbers the measurement script prints, computed the console's way. */
export interface CoordinationSummary {
  peakConcurrentTurns: number;
  sameAgentOverlaps: number;
  episodesOpened: number;
  episodesCompleted: number;
  /** Rounds per episode, in episode order. */
  roundsPerEpisode: number[];
  broadcasts: number;
  dms: number;
  referrals: number;
  /** Distinct `from→to` agent pairs that spoke, sorted. */
  pairs: string[];
}

/** Folds the two ledgers into one summary. */
export function coordinationSummary(frames: EpisodeFrames, ledger: TurnLedger): CoordinationSummary {
  const pairs = new Set<string>();
  let broadcasts = 0;
  let dms = 0;
  let referrals = 0;
  let completed = 0;
  const rounds: number[] = [];
  for (const observation of coordinationObservations(frames)) {
    if (observation.kind !== "spoke") continue;
    pairs.add(`${observation.from}→${observation.to}`);
    if (observation.via === "broadcast") broadcasts += 1;
    else if (observation.via === "dm") dms += 1;
    else referrals += 1;
  }
  for (const id of frames.order) {
    const episode = frames.byId[id];
    if (!episode) continue;
    if (episode.status === "completed") completed += 1;
    rounds.push(Math.max(episode.roundCount ?? 0, Object.keys(episode.rounds).length));
  }
  return {
    peakConcurrentTurns: ledger.peak,
    sameAgentOverlaps: ledger.sameAgentOverlaps,
    episodesOpened: frames.order.length,
    episodesCompleted: completed,
    roundsPerEpisode: rounds,
    broadcasts,
    dms,
    referrals,
    pairs: [...pairs].sort(),
  };
}
