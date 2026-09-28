// Coordination metrics over a company's `/events` stream, as pure functions.
//
// `scripts/measure-coordination.mjs` tails the SSE feed of a running host and
// folds every frame through `foldFrame`; `summarize` turns the fold into the
// numbers the run prints, and `evaluate` turns those into a verdict. All three
// are here, with no I/O, so `node --test scripts/lib/coordination-metrics.test.mjs`
// can state each rule in a few lines.
//
// The frames folded are the ones `docs/spec/runtime/events.md` names under
// "Hive episodes and rounds": `episode_opened`, `round_started`,
// `turn_started` / `turn_settled`, `round_committed`, `broadcast_routed`,
// `dm_delivered`, `episode_completed`, `referral`, plus `agent_reply` for the
// utterance-kind histogram. The console's `frontend/src/lib/coordination.ts`
// computes the same numbers over the same frames for the comms graph; the two
// are kept in step by hand.

/** A fresh, empty ledger. */
export function createLedger() {
  return {
    turns: {
      /** Open turns, by key. @type {Map<string, {agentId?: string, startedAt: number, episodeId?: string}>} */
      open: new Map(),
      /** @type {{agentId?: string, startedAt: number, settledAt: number, episodeId?: string}[]} */
      closed: [],
      peak: 0,
      sameAgentOverlaps: 0,
    },
    /** @type {Map<string, {id: string, chatId: string, openedAt?: number, completedAt?: number, status: "open"|"completed", rounds: Set<number>, roundCount?: number, reason?: string, plan?: any}>} */
    episodes: new Map(),
    /** @type {{from: string, to: string[], via: "broadcast"|"dm"|"referral", at: number, episodeId?: string, chatId: string}[]} */
    contacts: [],
    /** @type {Record<string, number>} */
    planKinds: {},
    /** @type {Record<string, number>} */
    routers: {},
    /** @type {Record<string, number>} from `round_committed`. */
    utteranceKinds: {},
    /** @type {Record<string, number>} from `agent_reply.episode`, the durable twin. */
    replyKinds: {},
    /** Referrals that crossed desks, keyed by the asking episode + target desk. */
    referrals: [],
    frames: 0,
  };
}

function episodeOf(ledger, id, chatId, at) {
  let episode = ledger.episodes.get(id);
  if (!episode) {
    episode = { id, chatId, openedAt: at, status: "open", rounds: new Set() };
    ledger.episodes.set(id, episode);
  }
  if (!episode.chatId && chatId) episode.chatId = chatId;
  return episode;
}

function turnKey(frame) {
  return frame.turnId ?? `${frame.agentId ?? "?"}:${frame.chatId ?? "?"}`;
}

/** The seats a routing plan hands a broadcast to, primary first. */
export function planTargets(plan) {
  const out = [];
  if (plan?.primaryId) out.push(plan.primaryId);
  for (const id of plan?.invitedIds ?? []) if (!out.includes(id)) out.push(id);
  return out;
}

/**
 * Folds one frame into the ledger. Mutates and returns it.
 *
 * @param {ReturnType<typeof createLedger>} ledger
 * @param {any} frame a parsed `/events` frame
 */
export function foldFrame(ledger, frame) {
  if (!frame || typeof frame.type !== "string") return ledger;
  ledger.frames += 1;
  const at = typeof frame.atMillis === "number" ? frame.atMillis : Date.now();
  switch (frame.type) {
    case "turn_started": {
      const { turns } = ledger;
      if (frame.agentId && [...turns.open.values()].some((turn) => turn.agentId === frame.agentId)) {
        turns.sameAgentOverlaps += 1;
      }
      turns.open.set(turnKey(frame), { agentId: frame.agentId, startedAt: at, episodeId: frame.episodeId });
      turns.peak = Math.max(turns.peak, turns.open.size);
      break;
    }
    case "turn_settled": {
      const { turns } = ledger;
      let key = turnKey(frame);
      if (!turns.open.has(key) && frame.agentId) {
        key = [...turns.open.entries()].find(([, turn]) => turn.agentId === frame.agentId)?.[0] ?? key;
      }
      const turn = turns.open.get(key);
      if (!turn) break;
      turns.open.delete(key);
      turns.closed.push({ ...turn, settledAt: at });
      break;
    }
    case "episode_opened": {
      const episode = episodeOf(ledger, frame.episodeId, frame.chatId, at);
      episode.openedAt = at;
      episode.plan = frame.plan;
      if (frame.plan?.kind) ledger.planKinds[frame.plan.kind] = (ledger.planKinds[frame.plan.kind] ?? 0) + 1;
      break;
    }
    case "round_started": {
      const episode = episodeOf(ledger, frame.episodeId, frame.chatId, at);
      episode.rounds.add(frame.revision);
      break;
    }
    case "round_committed": {
      const episode = episodeOf(ledger, frame.episodeId, frame.chatId, at);
      episode.rounds.add(frame.revision);
      for (const utterance of frame.utterances ?? []) {
        if (utterance?.kind) {
          ledger.utteranceKinds[utterance.kind] = (ledger.utteranceKinds[utterance.kind] ?? 0) + 1;
        }
      }
      break;
    }
    case "broadcast_routed": {
      episodeOf(ledger, frame.episodeId, frame.chatId, at);
      if (frame.router) ledger.routers[frame.router] = (ledger.routers[frame.router] ?? 0) + 1;
      if (frame.plan?.kind) ledger.planKinds[frame.plan.kind] = (ledger.planKinds[frame.plan.kind] ?? 0) + 1;
      const to = planTargets(frame.plan).filter((id) => id !== frame.agentId);
      ledger.contacts.push({ from: frame.agentId, to, via: "broadcast", at, episodeId: frame.episodeId, chatId: frame.chatId });
      break;
    }
    case "dm_delivered": {
      episodeOf(ledger, frame.episodeId, frame.chatId, at);
      const to = (frame.to ?? []).filter((id) => id !== frame.from);
      ledger.contacts.push({ from: frame.from, to, via: "dm", at, episodeId: frame.episodeId, chatId: frame.chatId });
      break;
    }
    case "referral": {
      if (frame.returning) break;
      const to = frame.direct ? frame.target : frame.toDesk;
      ledger.contacts.push({ from: frame.asker, to: [to], via: "referral", at, episodeId: frame.episodeId, chatId: frame.chatId });
      if (frame.toDesk && frame.toDesk !== frame.chatId) {
        ledger.referrals.push({ from: frame.chatId, to: frame.toDesk, asker: frame.asker, episodeId: frame.episodeId, toEpisodeId: frame.toEpisodeId, at });
      }
      break;
    }
    case "episode_completed": {
      const episode = episodeOf(ledger, frame.episodeId, frame.chatId, at);
      episode.status = "completed";
      episode.completedAt = at;
      episode.reason = frame.reason;
      episode.roundCount = frame.rounds;
      break;
    }
    case "agent_reply": {
      // The durable twin of `round_committed`'s kinds: one reply per
      // utterance, so a host that projects the episode onto the reply but
      // not the commit still yields a histogram.
      if (frame.episode?.kind) {
        ledger.replyKinds[frame.episode.kind] = (ledger.replyKinds[frame.episode.kind] ?? 0) + 1;
      }
      break;
    }
    default:
      break;
  }
  return ledger;
}

/** Whether every episode the ledger saw open has completed. At least one must have opened. */
export function allComplete(ledger) {
  if (ledger.episodes.size === 0) return false;
  for (const episode of ledger.episodes.values()) if (episode.status !== "completed") return false;
  return true;
}

/**
 * The most attempts open at once, from `GET /runs` rows — the cross-check for
 * the SSE bracket count. A row with no start is not an attempt that ran.
 *
 * @param {{startedAtMillis?: number, finishedAtMillis?: number, agentId?: string}[]} runs
 * @param {number} nowMillis
 */
export function peakFromRuns(runs, nowMillis = Date.now()) {
  const events = [];
  for (const run of runs) {
    if (typeof run.startedAtMillis !== "number") continue;
    events.push({ at: run.startedAtMillis, delta: 1 });
    events.push({ at: run.finishedAtMillis ?? nowMillis, delta: -1 });
  }
  // Ends before starts at the same instant: a turn that ends as another begins
  // is a hand-off, not an overlap.
  events.sort((a, b) => a.at - b.at || a.delta - b.delta);
  let open = 0;
  let peak = 0;
  for (const event of events) {
    open += event.delta;
    peak = Math.max(peak, open);
  }
  return peak;
}

/**
 * The numbers a run prints.
 *
 * @param {ReturnType<typeof createLedger>} ledger
 * @param {{now?: number}} [options]
 */
export function summarize(ledger, { now = Date.now() } = {}) {
  const pairs = new Set();
  let broadcasts = 0;
  let dms = 0;
  for (const contact of ledger.contacts) {
    if (contact.via === "broadcast") broadcasts += 1;
    else if (contact.via === "dm") dms += 1;
    for (const to of contact.to) pairs.add(`${contact.from}→${to}`);
  }
  const episodes = [...ledger.episodes.values()];
  const roundsPerEpisode = {};
  const timeToComplete = {};
  for (const episode of episodes) {
    roundsPerEpisode[episode.id] = Math.max(episode.roundCount ?? 0, episode.rounds.size);
    if (episode.status === "completed" && episode.openedAt !== undefined) {
      timeToComplete[episode.id] = (episode.completedAt ?? now) - episode.openedAt;
    }
  }
  const completed = episodes.filter((episode) => episode.status === "completed");
  return {
    frames: ledger.frames,
    maxConcurrentTurns: ledger.turns.peak,
    openTurns: ledger.turns.open.size,
    sameAgentOverlaps: ledger.turns.sameAgentOverlaps,
    episodesOpened: episodes.length,
    episodesCompleted: completed.length,
    episodesOpen: episodes.filter((episode) => episode.status !== "completed").map((episode) => `${episode.chatId}/${episode.id}`),
    roundsPerEpisode,
    broadcasts,
    dms,
    crossDeskReferrals: ledger.referrals.length,
    referralPairs: ledger.referrals.map((r) => `${r.from}→${r.to}`),
    planKinds: ledger.planKinds,
    routers: ledger.routers,
    utteranceKinds: Object.keys(ledger.utteranceKinds).length > 0 ? ledger.utteranceKinds : ledger.replyKinds,
    distinctPairs: [...pairs].sort(),
    timeToCompleteMillis: timeToComplete,
    reasons: Object.fromEntries(completed.map((episode) => [episode.id, episode.reason ?? "complete_episode"])),
  };
}

/** The thresholds a run must clear, as the plan states them. */
export const DEFAULT_THRESHOLDS = Object.freeze({
  maxConcurrentTurns: 2,
  crossDeskReferrals: 1,
  agentContacts: 1,
  distinctPairs: 2,
});

/**
 * The failures a summary carries against the thresholds — an empty list is a
 * pass, and the length is the script's exit code.
 *
 * @param {ReturnType<typeof summarize>} summary
 * @param {Partial<typeof DEFAULT_THRESHOLDS>} [thresholds]
 * @param {{runsPeak?: number}} [crossCheck]
 * @returns {string[]}
 */
export function evaluate(summary, thresholds = {}, crossCheck = {}) {
  const t = { ...DEFAULT_THRESHOLDS, ...thresholds };
  const failures = [];
  if (summary.maxConcurrentTurns < t.maxConcurrentTurns) {
    failures.push(`max concurrent turns ${summary.maxConcurrentTurns} < ${t.maxConcurrentTurns}`);
  }
  if (summary.sameAgentOverlaps > 0) {
    failures.push(`same-agent overlaps ${summary.sameAgentOverlaps} (must be 0)`);
  }
  if (summary.crossDeskReferrals < t.crossDeskReferrals) {
    failures.push(`cross-desk referrals ${summary.crossDeskReferrals} < ${t.crossDeskReferrals}`);
  }
  if (summary.broadcasts + summary.dms < t.agentContacts) {
    failures.push(`agent→agent dm/broadcast ${summary.broadcasts + summary.dms} < ${t.agentContacts}`);
  }
  if (summary.distinctPairs.length < t.distinctPairs) {
    failures.push(`distinct pairs ${summary.distinctPairs.length} < ${t.distinctPairs}`);
  }
  if (summary.episodesOpened === 0) {
    failures.push("no episode opened");
  } else if (summary.episodesOpen.length > 0) {
    failures.push(`${summary.episodesOpen.length} episode(s) never completed: ${summary.episodesOpen.join(", ")}`);
  }
  if (typeof crossCheck.runsPeak === "number" && crossCheck.runsPeak < t.maxConcurrentTurns) {
    failures.push(`GET /runs cross-check: peak ${crossCheck.runsPeak} < ${t.maxConcurrentTurns}`);
  }
  return failures;
}

/**
 * Parses one SSE event block into its JSON `data`, or null. A block is the
 * text between two blank lines; `data:` lines are joined with newlines.
 *
 * @param {string} block
 */
export function parseSseBlock(block) {
  const data = block
    .split(/\r?\n/)
    .filter((line) => line.startsWith("data:"))
    .map((line) => line.slice(5).replace(/^ /, ""))
    .join("\n");
  if (!data) return null;
  try {
    return JSON.parse(data);
  } catch {
    return null;
  }
}

/**
 * A stateful line splitter for an SSE body: feed it chunks, get whole blocks.
 */
export function createSseSplitter() {
  let buffer = "";
  return {
    /** @param {string} chunk @returns {string[]} the blocks completed by this chunk */
    push(chunk) {
      buffer += chunk.replace(/\r\n/g, "\n");
      const blocks = [];
      let at;
      while ((at = buffer.indexOf("\n\n")) !== -1) {
        const block = buffer.slice(0, at);
        buffer = buffer.slice(at + 2);
        if (block.trim()) blocks.push(block);
      }
      return blocks;
    },
  };
}
