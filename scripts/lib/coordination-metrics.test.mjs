// `node --test scripts/lib/coordination-metrics.test.mjs`
//
// The rules `measure-coordination.mjs` reports against, stated on canned
// frames: the bracket peak and the same-agent overlap count, the contacts a
// broadcast / dm / referral imply, the completion gate, and the thresholds.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  allComplete,
  createLedger,
  createSseSplitter,
  evaluate,
  foldFrame,
  parseSseBlock,
  peakFromRuns,
  planTargets,
  summarize,
} from "./coordination-metrics.mjs";

const fold = (frames) => frames.reduce(foldFrame, createLedger());

const bracket = (type, agentId, seq, extra = {}) => ({ type, seq, atMillis: seq * 10, chatId: "engineering", agentId, ...extra });

test("the bracket peak counts turns open at once, and same-agent overlaps stay zero", () => {
  const ledger = fold([
    bracket("turn_started", "engineer", 1, { turnId: "a" }),
    bracket("turn_started", "writer", 2, { turnId: "b" }),
    bracket("turn_settled", "engineer", 3, { turnId: "a" }),
    bracket("turn_started", "ceo", 4, { turnId: "c" }),
    bracket("turn_settled", "writer", 5, { turnId: "b" }),
    bracket("turn_settled", "ceo", 6, { turnId: "c" }),
  ]);
  assert.equal(ledger.turns.peak, 2);
  assert.equal(ledger.turns.open.size, 0);
  assert.equal(ledger.turns.sameAgentOverlaps, 0);
  assert.equal(ledger.turns.closed.length, 3);
});

test("a second start for an agent still running is the overlap the runtime forbids", () => {
  const ledger = fold([
    bracket("turn_started", "ceo", 1, { turnId: "a" }),
    bracket("turn_started", "ceo", 2, { turnId: "b" }),
  ]);
  assert.equal(ledger.turns.sameAgentOverlaps, 1);
  assert.equal(ledger.turns.peak, 2);
});

test("a settle without a turn id closes the agent's oldest open turn; a stray settle is ignored", () => {
  const ledger = fold([bracket("turn_started", "engineer", 1), bracket("turn_settled", "engineer", 2), bracket("turn_settled", "writer", 3)]);
  assert.equal(ledger.turns.open.size, 0);
  assert.equal(ledger.turns.closed.length, 1);
});

const EPISODE = [
  { type: "episode_opened", seq: 1, atMillis: 100, chatId: "engineering", episodeId: "ep-1", openedBySeq: 1, participants: ["engineer", "ceo"], plan: { kind: "hive", primaryId: "engineer", invitedIds: ["ceo"] } },
  { type: "round_started", seq: 2, atMillis: 110, chatId: "engineering", episodeId: "ep-1", revision: 0, agentIds: ["engineer", "ceo"] },
  { type: "round_committed", seq: 3, atMillis: 200, chatId: "engineering", episodeId: "ep-1", revision: 0, utterances: [{ agentId: "engineer", sequence: 4, kind: "post" }, { agentId: "ceo", sequence: 5, kind: "post" }] },
  { type: "round_started", seq: 4, atMillis: 210, chatId: "engineering", episodeId: "ep-1", revision: 1, agentIds: ["engineer", "ceo"] },
  { type: "broadcast_routed", seq: 5, atMillis: 300, chatId: "engineering", episodeId: "ep-1", revision: 1, agentId: "engineer", messageSeq: 7, plan: { kind: "hive", primaryId: "ceo", invitedIds: ["engineer", "ceo"] }, router: "jev" },
  { type: "dm_delivered", seq: 6, atMillis: 305, chatId: "engineering", episodeId: "ep-1", from: "ceo", to: ["engineer"], messageSeq: 8 },
  { type: "referral", seq: 7, atMillis: 310, chatId: "engineering", sequence: 7, toDesk: "content", target: "writer", asker: "engineer", direct: false, returning: false, episodeId: "ep-1", toEpisodeId: "ep-2" },
  { type: "referral", seq: 8, atMillis: 400, chatId: "engineering", sequence: 9, toDesk: "content", target: "writer", asker: "engineer", direct: false, returning: true, episodeId: "ep-1", toEpisodeId: "ep-2" },
  { type: "round_committed", seq: 9, atMillis: 410, chatId: "engineering", episodeId: "ep-1", revision: 1, utterances: [{ agentId: "engineer", sequence: 7, kind: "broadcast" }, { agentId: "ceo", sequence: 8, kind: "dm", to: ["engineer"] }] },
  { type: "episode_completed", seq: 10, atMillis: 500, chatId: "engineering", episodeId: "ep-1", revision: 3, completedBy: "ceo", rounds: 3, reason: "complete_episode" },
];

test("an episode's contacts, rounds, plans and completion fold into the summary", () => {
  const summary = summarize(fold(EPISODE), { now: 1000 });
  assert.equal(summary.episodesOpened, 1);
  assert.equal(summary.episodesCompleted, 1);
  assert.deepEqual(summary.episodesOpen, []);
  assert.deepEqual(summary.roundsPerEpisode, { "ep-1": 3 });
  assert.equal(summary.broadcasts, 1);
  assert.equal(summary.dms, 1);
  assert.equal(summary.crossDeskReferrals, 1);
  assert.deepEqual(summary.referralPairs, ["engineering→content"]);
  assert.deepEqual(summary.distinctPairs, ["ceo→engineer", "engineer→ceo", "engineer→content"]);
  assert.deepEqual(summary.planKinds, { hive: 2 });
  assert.deepEqual(summary.routers, { jev: 1 });
  assert.deepEqual(summary.utteranceKinds, { post: 2, broadcast: 1, dm: 1 });
  assert.deepEqual(summary.timeToCompleteMillis, { "ep-1": 400 });
  assert.deepEqual(summary.reasons, { "ep-1": "complete_episode" });
});

test("completion is gated on every opened episode, and on at least one", () => {
  assert.equal(allComplete(createLedger()), false);
  const ledger = fold(EPISODE.slice(0, 2));
  assert.equal(allComplete(ledger), false);
  foldFrame(ledger, EPISODE.at(-1));
  assert.equal(allComplete(ledger), true);
  // A second episode the referral opened on the far desk keeps the gate shut.
  foldFrame(ledger, { type: "episode_opened", seq: 11, atMillis: 600, chatId: "content", episodeId: "ep-2", openedBySeq: 9, participants: ["writer"], plan: { kind: "one", primaryId: "writer" } });
  assert.equal(allComplete(ledger), false);
});

test("the thresholds name each shortfall, and an empty list is a pass", () => {
  const good = summarize(fold([
    ...EPISODE,
    bracket("turn_started", "engineer", 20, { turnId: "a" }),
    bracket("turn_started", "ceo", 21, { turnId: "b" }),
    bracket("turn_settled", "engineer", 22, { turnId: "a" }),
    bracket("turn_settled", "ceo", 23, { turnId: "b" }),
  ]));
  assert.deepEqual(evaluate(good), []);
  assert.deepEqual(evaluate(good, {}, { runsPeak: 2 }), []);
  assert.deepEqual(evaluate(good, {}, { runsPeak: 1 }), ["GET /runs cross-check: peak 1 < 2"]);

  const empty = summarize(createLedger());
  assert.deepEqual(evaluate(empty), [
    "max concurrent turns 0 < 2",
    "cross-desk referrals 0 < 1",
    "agent→agent dm/broadcast 0 < 1",
    "distinct pairs 0 < 2",
    "no episode opened",
  ]);

  const stuck = summarize(fold([...EPISODE.slice(0, 9), bracket("turn_started", "ceo", 30, { turnId: "x" }), bracket("turn_started", "ceo", 31, { turnId: "y" })]));
  const failures = evaluate(stuck);
  assert.ok(failures.includes("same-agent overlaps 1 (must be 0)"), failures.join("; "));
  assert.ok(failures.some((f) => f.startsWith("1 episode(s) never completed: engineering/ep-1")), failures.join("; "));
});

test("the /runs cross-check counts overlapping attempts, treating a hand-off as no overlap", () => {
  assert.equal(
    peakFromRuns([
      { startedAtMillis: 0, finishedAtMillis: 10 },
      { startedAtMillis: 10, finishedAtMillis: 20 },
      { startedAtMillis: 5, finishedAtMillis: 8 },
      { createdAtMillis: 1 },
    ]),
    2,
  );
  assert.equal(peakFromRuns([{ startedAtMillis: 0 }, { startedAtMillis: 1 }], 5), 2);
});

test("plan targets are the primary then the invited, once each", () => {
  assert.deepEqual(planTargets({ kind: "hive", primaryId: "ceo", invitedIds: ["engineer", "ceo"] }), ["ceo", "engineer"]);
  assert.deepEqual(planTargets({ kind: "clarify" }), []);
  assert.deepEqual(planTargets(undefined), []);
});

test("SSE blocks split on blank lines and parse their data lines", () => {
  const splitter = createSseSplitter();
  assert.deepEqual(splitter.push("id: 1\ndata: {\"type\":\"a\"}\n\nid: 2\ndata: {\"ty"), ["id: 1\ndata: {\"type\":\"a\"}"]);
  assert.deepEqual(splitter.push("pe\":\"b\"}\r\n\r\n: keepalive\n\n"), ["id: 2\ndata: {\"type\":\"b\"}", ": keepalive"]);
  assert.deepEqual(parseSseBlock("id: 2\ndata: {\"type\":\"b\"}"), { type: "b" });
  assert.equal(parseSseBlock(": keepalive"), null);
  assert.equal(parseSseBlock("data: not json"), null);
});
