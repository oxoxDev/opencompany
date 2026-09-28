#!/usr/bin/env node
// Measure how a running company coordinates: post one task to a desk, tail
// `/events` until every episode it opened has completed, and print the numbers
// against the thresholds. Node 20+, no dependencies.
//
//   node scripts/measure-coordination.mjs --base http://127.0.0.1:8080 \
//       [--desk engineering] [--text "…"] [--seconds 600] [--mock] [--json]
//
// `--mock` prefixes the message with the mock brain's directives
// (`__MOCK_SLOW_MS__ 2000 __MOCK_DM__ ceo __MOCK_REFER__ engineer:content`) so
// the scripted run is slow enough to overlap and deterministic enough to
// assert on: the engineer's first post asks the content desk, which the host
// carries across as the referral the thresholds count. `--seconds` bounds the
// tail; a run that times out reports what it saw and fails the completion
// threshold. The exit code is the number of failed thresholds.
//
// This is the HTTP/SSE twin of `opencompany measure` (which reads the store
// directly): the same numbers, computed from what a console would see, so a
// hosted tenant can be measured with nothing but a URL and an admin address.
// `scripts/measure-coordination.sh` boots the mock brain and a host for it.
//
// Sign-in is the magic-link flow with the echoed `dev_code`, exactly as
// `frontend/test/e2e/global-setup.ts` does it: the host must bind loopback
// and have no mail transport, or the code is mailed and this cannot read it.

import { parseArgs } from "node:util";

import {
  allComplete,
  createLedger,
  createSseSplitter,
  DEFAULT_THRESHOLDS,
  evaluate,
  foldFrame,
  parseSseBlock,
  peakFromRuns,
  summarize,
} from "./lib/coordination-metrics.mjs";

const { values: args } = parseArgs({
  options: {
    base: { type: "string", default: process.env.OPENCOMPANY_BASE_URL ?? "http://127.0.0.1:8080" },
    email: { type: "string", default: process.env.OPENCOMPANY_ADMIN_EMAIL ?? "harness-e2e@tinyhumans.ai" },
    desk: { type: "string", default: "engineering" },
    text: {
      type: "string",
      default:
        "Plan the staging rollout for the new checkout, and get the content desk to draft the release note.",
    },
    seconds: { type: "string", default: "600" },
    mock: { type: "boolean", default: false },
    json: { type: "boolean", default: false },
    quiet: { type: "boolean", default: false },
  },
});

const base = args.base.replace(/\/+$/, "");
const scope = `${base}/api/v1/company`;
const deadlineMillis = Number.parseInt(args.seconds, 10) * 1000;
const log = (line) => {
  if (!args.quiet) process.stderr.write(`[measure] ${line}\n`);
};

/** A cookie jar the size of this script: the session cookie and nothing else. */
const jar = new Map();
function remember(response) {
  for (const header of response.headers.getSetCookie?.() ?? []) {
    const [pair] = header.split(";");
    const at = pair.indexOf("=");
    if (at > 0) jar.set(pair.slice(0, at).trim(), pair.slice(at + 1).trim());
  }
}
function cookie() {
  return [...jar.entries()].map(([k, v]) => `${k}=${v}`).join("; ");
}
async function call(method, path, body) {
  const response = await fetch(`${scope}${path}`, {
    method,
    headers: {
      "content-type": "application/json",
      accept: "application/json",
      ...(jar.size ? { cookie: cookie() } : {}),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  remember(response);
  const text = await response.text();
  let json = null;
  try {
    json = text ? JSON.parse(text) : null;
  } catch {
    json = null;
  }
  return { response, json, text };
}

async function signIn() {
  const requested = await call("POST", "/auth/request", { email: args.email });
  if (!requested.response.ok) {
    throw new Error(`auth/request answered ${requested.response.status}: ${requested.text.slice(0, 200)}`);
  }
  const code = requested.json?.dev_code;
  if (typeof code !== "string" || !code) {
    throw new Error(
      `auth/request echoed no dev_code for ${args.email}. The company must list that address under ` +
        "[users] admins (or OPENCOMPANY_ADMIN_EMAIL), the host must bind loopback, and no mail transport may be set.",
    );
  }
  const verified = await call("POST", "/auth/verify", { code });
  if (!verified.response.ok) {
    throw new Error(`auth/verify answered ${verified.response.status}: ${verified.text.slice(0, 200)}`);
  }
  log(`signed in as ${args.email}`);
}

/**
 * Opens `/events` and folds every frame until `until()` says stop or the
 * deadline passes. Resolves with whether it stopped because `until()` did.
 */
async function tail(ledger, until, millis) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), millis);
  let satisfied = false;
  try {
    const response = await fetch(`${scope}/events`, {
      headers: { accept: "text/event-stream", ...(jar.size ? { cookie: cookie() } : {}) },
      signal: controller.signal,
    });
    if (!response.ok || !response.body) {
      throw new Error(`/events answered ${response.status}`);
    }
    const splitter = createSseSplitter();
    const decoder = new TextDecoder();
    for await (const chunk of response.body) {
      for (const block of splitter.push(decoder.decode(chunk, { stream: true }))) {
        const frame = parseSseBlock(block);
        if (!frame) continue;
        foldFrame(ledger, frame);
        if (!args.quiet && isInteresting(frame)) log(describe(frame));
        if (until(ledger)) {
          satisfied = true;
          controller.abort();
          break;
        }
      }
      if (satisfied) break;
    }
  } catch (error) {
    if (!(error instanceof Error && error.name === "AbortError")) throw error;
  } finally {
    clearTimeout(timer);
  }
  return satisfied;
}

const INTERESTING = new Set([
  "episode_opened",
  "round_started",
  "round_committed",
  "broadcast_routed",
  "dm_delivered",
  "episode_completed",
  "referral",
  "turn_started",
  "turn_settled",
]);
const isInteresting = (frame) => INTERESTING.has(frame.type);
function describe(frame) {
  switch (frame.type) {
    case "episode_opened":
      return `episode ${frame.episodeId} opened on #${frame.chatId} plan=${frame.plan?.kind} seats=${(frame.participants ?? []).join(",")}`;
    case "round_started":
      return `  round ${frame.revision} of ${frame.episodeId}: ${(frame.agentIds ?? []).join(" + ")}`;
    case "turn_started":
      return `    ${frame.agentId ?? "?"} started${frame.episodeId ? ` (r${frame.roundRevision})` : ""}`;
    case "turn_settled":
      return `    ${frame.agentId ?? "?"} settled ${frame.outcome ?? ""}`;
    case "round_committed":
      return `  round ${frame.revision} committed: ${(frame.utterances ?? []).map((u) => `${u.agentId}:${u.kind}`).join(" ")}`;
    case "broadcast_routed":
      return `  broadcast by ${frame.agentId} → ${frame.plan?.kind}/${frame.plan?.primaryId ?? ""} via ${frame.router}`;
    case "dm_delivered":
      return `  dm ${frame.from} → ${(frame.to ?? []).join(",")}`;
    case "referral":
      return `  referral ${frame.returning ? "returned" : "asked"} ${frame.chatId} → ${frame.toDesk}${frame.toEpisodeId ? ` (${frame.toEpisodeId})` : ""}`;
    case "episode_completed":
      return `episode ${frame.episodeId} completed after ${frame.rounds} round(s): ${frame.reason}`;
    default:
      return frame.type;
  }
}

async function main() {
  await signIn();
  const ledger = createLedger();
  const text = args.mock
    ? `__MOCK_SLOW_MS__ 2000 __MOCK_DM__ ceo __MOCK_REFER__ ${args.desk === "content" ? "writer:engineering" : "engineer:content"} ${args.text}`
    : args.text;

  // Open the tail first, then post: a frame emitted before the stream is
  // attached is a frame nobody counts, and `episode_opened` is the first one.
  const started = Date.now();
  const tailing = tail(ledger, (l) => allComplete(l) && l.turns.open.size === 0, deadlineMillis);
  await new Promise((resolve) => setTimeout(resolve, 500));
  const posted = await call("POST", "/chat", { text, chat: args.desk, detach: true });
  if (!posted.response.ok) {
    throw new Error(`POST /chat answered ${posted.response.status}: ${posted.text.slice(0, 300)}`);
  }
  log(`posted to #${args.desk}: ${text}`);
  const completed = await tailing;
  const elapsed = Date.now() - started;
  log(completed ? `every episode completed in ${(elapsed / 1000).toFixed(1)}s` : `deadline reached after ${(elapsed / 1000).toFixed(1)}s`);

  // The durable cross-check: `GET /runs` rows carry start/finish stamps, so
  // the SSE bracket peak can be confirmed against something a reload sees.
  let runsPeak;
  const runs = await call("GET", "/runs?limit=200");
  if (runs.response.ok && Array.isArray(runs.json)) {
    const since = runs.json.filter((run) => (run.startedAtMillis ?? run.createdAtMillis ?? 0) >= started - 5000);
    runsPeak = peakFromRuns(since);
  } else if (runs.response.ok && Array.isArray(runs.json?.runs)) {
    runsPeak = peakFromRuns(runs.json.runs);
  }

  const summary = summarize(ledger);
  const failures = evaluate(summary, DEFAULT_THRESHOLDS, { runsPeak });
  const report = { base, desk: args.desk, elapsedMillis: elapsed, completed, runsPeak, thresholds: DEFAULT_THRESHOLDS, summary, failures };
  if (args.json) {
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  } else {
    const lines = [
      `max concurrent turns      ${summary.maxConcurrentTurns}${runsPeak !== undefined ? ` (GET /runs: ${runsPeak})` : ""}`,
      `same-agent overlaps       ${summary.sameAgentOverlaps}`,
      `episodes                  ${summary.episodesCompleted}/${summary.episodesOpened} completed`,
      `rounds per episode        ${Object.entries(summary.roundsPerEpisode).map(([id, n]) => `${id}=${n}`).join(" ") || "-"}`,
      `broadcasts / dms          ${summary.broadcasts} / ${summary.dms}`,
      `cross-desk referrals      ${summary.crossDeskReferrals} ${summary.referralPairs.join(" ")}`,
      `distinct pairs            ${summary.distinctPairs.length} ${summary.distinctPairs.join(" ")}`,
      `plan kinds                ${JSON.stringify(summary.planKinds)}`,
      `routers                   ${JSON.stringify(summary.routers)}`,
      `utterance kinds           ${JSON.stringify(summary.utteranceKinds)}`,
      `time to complete (ms)     ${JSON.stringify(summary.timeToCompleteMillis)}`,
      `reasons                   ${JSON.stringify(summary.reasons)}`,
      "",
      failures.length === 0 ? "PASS: every threshold met" : `FAIL (${failures.length}):\n  - ${failures.join("\n  - ")}`,
    ];
    process.stdout.write(`${lines.join("\n")}\n`);
  }
  process.exit(failures.length);
}

main().catch((error) => {
  process.stderr.write(`[measure] ${error instanceof Error ? error.message : String(error)}\n`);
  process.exit(99);
});
