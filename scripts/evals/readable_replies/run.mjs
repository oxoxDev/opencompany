#!/usr/bin/env node
// Readable-replies eval: send fixed prompts to a running company, read every
// reply back from the chat history, and report how long replies are and how
// often internal ids, tool names or protocol text reach them. Node 20+, no
// dependencies. Opt-in: nothing in CI runs it. See README.md.
//
//   node scripts/evals/readable_replies/run.mjs --base http://127.0.0.1:8280 \
//       [--samples 3] [--seconds 240] [--json]
//
// Each reply is scored twice: the raw text the journal keeps (`cueText` when
// the display projection changed it, else `text`) and the projected text a
// person is shown (`text`). The exit code is 0 when the projected text has no
// leak hits, 1 when it has some, 99 on a harness error.

import { parseArgs } from "node:util";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

const { values: args } = parseArgs({
  options: {
    base: { type: "string", default: process.env.OPENCOMPANY_BASE_URL ?? "http://127.0.0.1:8280" },
    email: { type: "string", default: "harness-e2e@tinyhumans.ai" },
    company: { type: "string", default: join(here, "company") },
    samples: { type: "string", default: "3" },
    seconds: { type: "string", default: "240" },
    json: { type: "boolean", default: false },
  },
});

const base = args.base.replace(/\/+$/, "");
const scope = `${base}/api/v1/company`;
const log = (line) => process.stderr.write(`[readable] ${line}\n`);

function positiveInt(name, raw) {
  const value = Number.parseInt(raw, 10);
  if (!Number.isInteger(value) || value <= 0 || String(value) !== raw.trim()) {
    log(`--${name} must be a positive integer, got ${JSON.stringify(raw)}`);
    process.exit(99);
  }
  return value;
}

const samples = positiveInt("samples", args.samples);
const waitMillis = positiveInt("seconds", args.seconds) * 1000;

if (!/^https?:\/\/(127\.0\.0\.1|localhost|\[::1\])(:\d+)?$/.test(base)) {
  log(`refusing ${base}: this eval signs in with an echoed dev code and only runs against a loopback host`);
  process.exit(99);
}

/**
 * The fixed prompts. `chat` is a desk id or, for a DM, the teammate's id.
 * `followUp` is sent on the same chat after the first reply lands, so the
 * seat answers again on a seeded turn.
 */
const PROMPTS = [
  { chat: "product_lead", text: "Who should I talk to about a flaky checkout test?" },
  { chat: "product_lead", text: "What is the team working on this week?" },
  { chat: "product_lead", text: "Can someone draft a two-line release note for the new search?" },
  { chat: "qa_engineer", text: "Is the payments release safe to ship today?" },
  { chat: "backend_dev", text: "How would you add rate limiting to the public API?" },
  { chat: "copy_writer", text: "Give me a subject line for the launch email." },
  {
    chat: "build_desk",
    text: "The nightly build failed on the payments tests. What do we do?",
    followUp: "Thanks. Who owns the fix, and when will we know?",
  },
  {
    chat: "build_desk",
    text: "Plan the testing for the new checkout flow.",
    followUp: "Shorter please: just the first three steps.",
  },
  {
    chat: "launch_desk",
    text: "When should we announce the new search, and what do we say?",
    followUp: "Good. Who writes it and who signs off?",
  },
  {
    chat: "launch_desk",
    text: "Draft the changelog entry for the rate limiting work.",
    followUp: "Make it friendlier for customers.",
  },
  { chat: "build_desk", text: "Summarise what this desk decided today." },
  { chat: "launch_desk", text: "Is anything blocking the launch?" },
];

/** Ids, read off the fixture company, that must never reach a person. */
function fixtureIds(dir) {
  const ids = new Set();
  const manifest = readFileSync(join(dir, "company.toml"), "utf8");
  for (const match of manifest.matchAll(/^\s*id\s*=\s*"([^"]+)"/gm)) ids.add(match[1]);
  for (const file of readdirSync(join(dir, "agents"))) {
    if (file.endsWith(".toml")) ids.add(file.slice(0, -".toml".length));
  }
  return [...ids];
}

const TOOL_NAMES = [
  "delegate_to_desk",
  "delegate_to_teammate",
  "spawn_task",
  "query_company",
  "list_tasks",
  "assign_task",
  "review_task",
  "run_workflow",
  "create_workflow",
  "escalate_to_human",
  "request_approval",
  "memory_store",
  "memory_recall",
  "memory_forget",
  "complete_episode",
  "desk_broadcast",
  "desk_ask",
  "desk_read",
  "desk_complete_episode",
];

const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

function leakPatterns(ids) {
  return {
    roster_or_desk_id: new RegExp(`(^|[^\\w/.-])@?\`?(${ids.map(escape).join("|")})\`?(?![\\w-])`, "g"),
    tool_name: new RegExp(`\\b(${TOOL_NAMES.map(escape).join("|")})\\b`, "g"),
    card_or_task_id: /\b(card|task)[ _#-]*\d+\b|\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi,
    run_id: /\brun_id\b|\brun[_-][0-9a-z]{6,}\b/gi,
    sequence_number: /\bsequence \d+\b|\bseq(uence)?[ #:]*\d{2,}\b/gi,
    conversation_prefix: /\[conversation:/g,
    bare_json: null,
  };
}

/** Lines outside fenced code, which is where a leak counts. */
function proseLines(text) {
  const out = [];
  let open = null;
  for (const line of text.split("\n")) {
    const fence = /^\s{0,3}(`{3,}|~{3,})(.*)$/.exec(line);
    if (open === null) {
      if (fence && !(fence[1][0] === "`" && fence[2].includes("`"))) {
        open = fence[1];
        continue;
      }
      out.push(line);
    } else if (fence && fence[1][0] === open[0] && fence[1].length >= open.length && fence[2].trim() === "") {
      open = null;
    }
  }
  return out;
}

function bareJson(text) {
  return proseLines(text).filter((line) => /^\s*[[{]/.test(line) && /"[\w-]+"\s*:/.test(line)).length;
}

function score(text, patterns) {
  const prose = proseLines(text).join("\n");
  const hits = {};
  for (const [name, pattern] of Object.entries(patterns)) {
    if (pattern === null) continue;
    hits[name] = [...prose.matchAll(pattern)].length;
  }
  hits.bare_json = bareJson(text);
  const words = text.split(/\s+/).filter(Boolean).length;
  const lines = text.split("\n").filter((line) => line.trim()).length;
  return { words, lines, hits };
}

function percentile(values, p) {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1)];
}

const jar = new Map();
function remember(response) {
  for (const header of response.headers.getSetCookie?.() ?? []) {
    const [pair] = header.split(";");
    const at = pair.indexOf("=");
    if (at > 0) jar.set(pair.slice(0, at).trim(), pair.slice(at + 1).trim());
  }
}
async function call(method, path, body) {
  const response = await fetch(`${scope}${path}`, {
    method,
    headers: {
      "content-type": "application/json",
      accept: "application/json",
      ...(jar.size ? { cookie: [...jar].map(([k, v]) => `${k}=${v}`).join("; ") } : {}),
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
  const code = requested.json?.dev_code;
  if (!requested.response.ok || typeof code !== "string") {
    throw new Error(`auth/request gave no dev_code (${requested.response.status}); see README.md "Running it"`);
  }
  const verified = await call("POST", "/auth/verify", { code });
  if (!verified.response.ok) throw new Error(`auth/verify answered ${verified.response.status}`);
}

async function history(chat) {
  const read = await call("GET", `/chat/history?desk=${encodeURIComponent(chat)}&limit=200`);
  if (!read.response.ok) throw new Error(`chat/history for ${chat} answered ${read.response.status}`);
  const rows = Array.isArray(read.json) ? read.json : (read.json?.messages ?? []);
  return rows.filter((row) => row.byPerson === false || (row.byPerson === undefined && row.mine === false));
}

/** Posts `text` to `chat` and waits until a reply newer than `after` lands. */
async function ask(chat, text, after) {
  const posted = await call("POST", "/chat", { text, chat, detach: true });
  if (!posted.response.ok) throw new Error(`POST /chat to ${chat} answered ${posted.response.status}`);
  const deadline = Date.now() + waitMillis;
  let seen = [];
  let quietSince = Date.now();
  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 3000));
    const fresh = (await history(chat)).filter((row) => Number(row.id) > after);
    if (fresh.length !== seen.length) {
      seen = fresh;
      quietSince = Date.now();
    } else if (seen.length > 0 && Date.now() - quietSince > 15000) {
      break;
    }
  }
  if (seen.length === 0) {
    throw new Error(`no reply from ${chat} to ${JSON.stringify(text)} within ${waitMillis}ms`);
  }
  return seen;
}

async function main() {
  const ids = fixtureIds(args.company);
  const patterns = leakPatterns(ids);
  await signIn();
  log(`signed in; ${PROMPTS.length} prompts x ${samples} samples against ${base}`);

  const replies = [];
  for (let sample = 1; sample <= samples; sample += 1) {
    for (const prompt of PROMPTS) {
      const before = Math.max(0, ...(await history(prompt.chat)).map((row) => Number(row.id)));
      const turns = [{ text: prompt.text, turn: 1 }];
      if (prompt.followUp) turns.push({ text: prompt.followUp, turn: 2 });
      let after = before;
      for (const turn of turns) {
        const rows = await ask(prompt.chat, turn.text, after);
        for (const row of rows) {
          const projected = row.text ?? "";
          const raw = row.cueText ?? projected;
          replies.push({
            sample,
            chat: prompt.chat,
            turn: turn.turn,
            author: row.author ?? row.channel,
            raw: score(raw, patterns),
            projected: score(projected, patterns),
            text: projected,
          });
          after = Math.max(after, Number(row.id));
        }
        log(`sample ${sample} ${prompt.chat} turn ${turn.turn}: ${rows.length} repl${rows.length === 1 ? "y" : "ies"}`);
      }
    }
  }

  const total = (side) => {
    const sums = {};
    for (const reply of replies) {
      for (const [name, count] of Object.entries(reply[side].hits)) sums[name] = (sums[name] ?? 0) + count;
    }
    return sums;
  };
  const words = replies.map((reply) => reply.projected.words);
  const lines = replies.map((reply) => reply.projected.lines);
  const report = {
    base,
    replies: replies.length,
    words: { p50: percentile(words, 50), p90: percentile(words, 90) },
    lines: { p50: percentile(lines, 50), p90: percentile(lines, 90) },
    leaks: { raw: total("raw"), projected: total("projected") },
    samples: replies,
  };
  const leaking = Object.values(report.leaks.projected).reduce((a, b) => a + b, 0);

  if (args.json) {
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  } else {
    const row = (name) => `  ${name.padEnd(22)} raw ${String(report.leaks.raw[name] ?? 0).padStart(4)}   shown ${String(report.leaks.projected[name] ?? 0).padStart(4)}`;
    process.stdout.write(
      [
        `replies                  ${report.replies}`,
        `words  p50 / p90         ${report.words.p50} / ${report.words.p90}`,
        `lines  p50 / p90         ${report.lines.p50} / ${report.lines.p90}`,
        "leak hits:",
        ...Object.keys(report.leaks.raw).map(row),
        leaking === 0 ? "PASS: nothing leaked into what a person is shown" : `FAIL: ${leaking} leak hit(s) in shown text`,
        "",
      ].join("\n"),
    );
  }
  process.exit(leaking === 0 ? 0 : 1);
}

main().catch((error) => {
  log(error instanceof Error ? error.message : String(error));
  process.exit(99);
});
