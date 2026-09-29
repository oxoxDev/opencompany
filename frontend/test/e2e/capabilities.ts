/**
 * What the host under test can actually do, so a spec that needs more than the
 * default build can say so instead of failing (issue #428).
 *
 * # Why these are opt-in variables and not a probe of the host
 *
 * A probe would be better, and there is very nearly one: `GET /tiny` reports
 * the vendored runtime modules. It cannot be used for this. `openhuman` is
 * reported as `enabled: true` unconditionally — the field describes the
 * vendored *checkout*, not the `openhuman` cargo feature — so a default build
 * answers exactly like a feature-gated one. A probe that cannot distinguish the
 * two would skip nothing, or skip everything, and would look authoritative
 * while doing it.
 *
 * So the caller declares what it brought. That is honest about where the
 * knowledge lives: whoever built the binary and started the inference backend
 * is the only party that knows.
 *
 * # The skips now have a lane behind them (issue #467)
 *
 * Four of the suite's best specs sit behind `LIVE_BRAIN`, and they are the ones
 * that exercise the product rather than the console's own rendering. CI runs
 * them: `Console E2E (live brain)` builds `--features openhuman,mcp`
 * and `playwright.config.ts` stands up `mock-brain.mjs` and `mcp-server.mjs`
 * behind it. The default-feature `Console E2E` lane still skips them, because
 * on a host without the harness the thing they test is not compiled in — that
 * skip is a true statement about that host, not a debt.
 */

/**
 * A host built with `--features openhuman,mcp` **and** an inference
 * backend behind it — either the mock that echoes `__MOCK_LLM__` or one whose
 * tool choices are scripted (`SPAWNONE`).
 *
 * Set `PW_LIVE_BRAIN=1` when both are true. Without them the agent harness is
 * not compiled in at all, so a sent message is answered by nothing, a workflow
 * node with no inference source never runs, and no orchestrator exists to open
 * a card.
 */
export const LIVE_BRAIN = process.env.PW_LIVE_BRAIN === "1";

/**
 * Whether the run brings up its own host, as opposed to driving one you started
 * and named with `PW_BASE_URL`. Mirrors `playwright.config.ts`'s `managesHost`,
 * and settles the same question for the fixtures: a host this run launched was
 * pointed at fixtures this run also launched, so their addresses are known here
 * without being restated.
 */
const MANAGES_HOST = !process.env.PW_BASE_URL;

/**
 * Where `mock-brain.mjs` listens when this run starts it. The host is handed
 * `http://…/v1` as its `OPENCOMPANY_INFERENCE_URL`; override with
 * `PW_MOCK_BRAIN_BIND` if 8099 is taken.
 */
export const MOCK_BRAIN_BIND = process.env.PW_MOCK_BRAIN_BIND || "127.0.0.1:8099";

/** Where `mcp-server.mjs` listens when this run starts it. */
export const MCP_FIXTURE_BIND = process.env.PW_MCP_FIXTURE_BIND || "127.0.0.1:8098";

/**
 * A host built with `--features composio`, pointed at
 * [`composio-backend.mjs`](./composio-backend.mjs) rather than the platform.
 *
 * Set `PW_COMPOSIO=1` when both are true. The default-feature binary compiles
 * none of the live Composio plane — every route on it answers `409 not in this
 * build` — so a spec about *which connected account an agent acts as* has
 * nothing to drive there. A declaration rather than a probe, for the same
 * reason {@link LIVE_BRAIN} is one: the person who chose the feature set is the
 * only one who knows.
 */
export const COMPOSIO = process.env.PW_COMPOSIO === "1";

/** Where `composio-backend.mjs` listens when this run starts it. */
export const COMPOSIO_FIXTURE_BIND = process.env.PW_COMPOSIO_FIXTURE_BIND || "127.0.0.1:8097";

/**
 * The fixture's base URL, for a spec that reads back what the host sent it.
 * Defaulted only when this run started it — against a host you brought, the
 * backend it dials is yours to name.
 */
export const COMPOSIO_FIXTURE_URL =
  process.env.PW_COMPOSIO_FIXTURE_URL ||
  (COMPOSIO && MANAGES_HOST ? `http://${COMPOSIO_FIXTURE_BIND}` : undefined);

/** The reason string a `COMPOSIO` skip carries, so no skip is ever bare. */
export const COMPOSIO_REASON =
  "needs a --features composio host pointed at test/e2e/composio-backend.mjs; " +
  "set PW_COMPOSIO=1 to run (issue #820).";

/**
 * The **URL** of an MCP server an agent may be told to call.
 *
 * `PW_MCP_SERVER` was retired in #414, and rightly: it carried a path to a
 * *stdio* server, which this host rejects outright (`src/company/mcp.rs` refuses
 * any declaration with a `command`), so it gated a spec that could not have
 * passed even had the fixture existed. `mcp.spec.ts` now drives the console
 * surface against a default host and needs nothing.
 *
 * It comes back here carrying a URL, for the half that page cannot reach: an
 * *agent* calling a tool on a real server over the real transport
 * (`mcp-agent.spec.ts`), which needs the harness and therefore the live-brain
 * lane. That lane starts `mcp-server.mjs`, which is why this is defaulted only
 * when the run manages the host. Against a host you brought, name your own.
 */
export const MCP_SERVER =
  process.env.PW_MCP_SERVER ||
  (LIVE_BRAIN && MANAGES_HOST ? `http://${MCP_FIXTURE_BIND}/mcp` : undefined);

/** The reason string a `LIVE_BRAIN` skip carries, so no skip is ever bare. */
export const LIVE_BRAIN_REASON =
  "needs a --features openhuman,mcp host plus an inference backend; " +
  "set PW_LIVE_BRAIN=1 to run. The `Console E2E (live brain)` CI lane does " +
  "(issue #467).";

/**
 * A host built with the harness features and pointed at a **real model**
 * (`test/e2e/live-brain-proxy.mjs`) rather than at the scripted mock.
 *
 * Set `PW_LIVE_LLM=1`, or run `npm run e2e:live-llm`, which sets it and — when
 * this config manages the host — starts the proxy and points the host at it.
 *
 * ## Why this is a run of its own rather than a flag inside a spec
 *
 * The same reason {@link FIRST_RUN} is: the two lanes need different hosts.
 * Every other spec in this suite asserts on the mock's scripted answers, and a
 * host whose inference is a real model answers none of them — a `__MOCK_LLM__`
 * marker no longer appears, a `SPAWNONE` no longer opens exactly one card.
 * Conversely `orchestration-live.spec.ts` asserts that a model *decided*
 * something, which is unpassable against a fixture that decides nothing. So
 * `playwright.config.ts` selects that spec only in this run, and every other
 * spec only outside one, and neither can be pointed at a host it cannot pass
 * against.
 *
 * The lane is **not** run by CI, and deliberately: it spends real tokens and its
 * verdict depends on a model's judgement, which is the one thing a required
 * check must not. It exists to be run by a person — before changing an
 * orchestrator prompt, a tool description, or the delegation seam — and its
 * scripted twin, `orchestration-simulation.spec.ts`, is what guards the same
 * chain on every push.
 */
export const LIVE_LLM = process.env.PW_LIVE_LLM === "1";

/** Where `live-brain-proxy.mjs` listens when this run starts it. */
export const LIVE_LLM_BIND = process.env.PW_LIVE_LLM_BIND || "127.0.0.1:8096";

/** The reason string a `LIVE_LLM` skip carries, so no skip is ever bare. */
export const LIVE_LLM_REASON =
  "needs a --features openhuman,mcp host pointed at a real model; " +
  "run `npm run e2e:live-llm` (which sets PW_LIVE_LLM=1 and starts " +
  "test/e2e/live-brain-proxy.mjs in front of the configured router).";

/**
 * Whether this run's host serves the **unstaffed first-run fixture**
 * (`companies/e2e_setup`) rather than the suite's usual harness company.
 *
 * Set `PW_FIRST_RUN=1`, or run `npm run e2e:first-run`, which sets it and — when
 * this config manages the host — points that host at the fixture and at a data
 * root of its own.
 *
 * ## Why this is a separate run and not a skip inside the spec
 *
 * First-run setup opens only on a company nobody has staffed, and every company
 * under `companies/` except this fixture declares a roster of its own. So
 * `company-setup.spec.ts` cannot pass against the harness company and the
 * harness company cannot serve the rest of the suite — two hosts, therefore two
 * runs.
 *
 * It used to be one run with a `test.skip` inside the spec, and that is the
 * shape issue #1404 was half about: the guard read the roster, found the global
 * baseline on it, and skipped **every time**, so the lane reported green over a
 * feature that could not open at all. `CLAUDE.md` names this exact pathology for
 * Rust targets — "builds, runs and reports zero without failing anything".
 *
 * The replacement has three parts, and all three are needed:
 *
 *   1. {@link playwright.config.ts} selects the first-run spec **only** in a
 *      first-run run, and every other spec **only** outside one, so neither can
 *      be pointed at a host it cannot pass against;
 *   2. the spec asserts its host is the right one instead of skipping, so a
 *      mispointed run fails loudly on the first line rather than quietly on all
 *      of them;
 *   3. `Console E2E (first run)` in CI runs it, through
 *      `scripts/ci/assert-e2e-spec-ran.sh`, which fails on a reported count of
 *      zero — a number, because a configuration can look right and be vacuous.
 */
export const FIRST_RUN = process.env.PW_FIRST_RUN === "1";

/** The company a first-run run must be serving, relative to the repository root. */
export const FIRST_RUN_COMPANY = "companies/e2e_setup";

/**
 * Whether this run is the **Project Euler lane**: the live-LLM host, but
 * serving `companies/math_lab` and running the one spec whose verdict
 * is a published integer rather than a shape on the board.
 *
 * Set `PW_EULER=1` alongside `PW_LIVE_LLM=1`, or run `npm run e2e:euler`, which
 * sets both and — when this config manages the host — points it at that company
 * and at a data root of its own.
 *
 * ## Why a lane rather than one more spec in the live-LLM run
 *
 * The same reason {@link FIRST_RUN} and {@link LIVE_LLM} are lanes: it needs a
 * different host. `orchestration-live.spec.ts` drives the harness company,
 * whose roster is a CEO, an engineer and a writer; this spec drives a lab whose
 * roster, tool grants and *withheld* tool grants are the thing being exercised.
 * Neither company can serve the other's spec.
 *
 * It also wants a data root of its own, for the reason the first-run lane does:
 * the answers ledger is read at the end of a run, and a root still holding the
 * previous run's rows would let a stale answer pass for a fresh one.
 *
 * Like the live-LLM lane it is **not** run by CI — real tokens, tens of
 * minutes, and a verdict that depends on a model's reasoning.
 */
export const EULER = process.env.PW_EULER === "1";

/** The company a Project Euler run must be serving, relative to the repository root. */
export const EULER_COMPANY = "companies/math_lab";

/** The reason string a `EULER` skip carries, so no skip is ever bare. */
export const EULER_REASON =
  "needs a host serving companies/math_lab and thinking with a real model; " +
  "run `npm run e2e:euler` (which sets PW_EULER=1 and PW_LIVE_LLM=1). " +
  "Point it at another problem with PW_EULER_PROBLEM=<number>.";

/**
 * Whether this run is the **hive lane**: the live-brain host (mock brain,
 * `--features openhuman,mcp`) but serving `companies/hive_demo` — two desks
 * sharing a seat — and running the one spec that watches a desk answer as a
 * room: two lanes working at once, a dm chip, the completion marker.
 *
 * Set `PW_HIVE=1` alongside `PW_LIVE_BRAIN=1`, or run `npm run e2e:hive`,
 * which sets both and — when this config manages the host — points it at
 * that company and at a data root of its own.
 *
 * A lane rather than one more live-brain spec for the reason {@link EULER}
 * is: it needs a different company. The harness company's desks have one
 * member each, and a one-member desk runs no round.
 */
export const HIVE = process.env.PW_HIVE === "1";

/** The company a hive run must be serving, relative to the repository root. */
export const HIVE_COMPANY = "companies/hive_demo";

/** The reason string a `HIVE` skip carries, so no skip is ever bare. */
export const HIVE_REASON =
  "needs a --features openhuman,mcp host serving companies/hive_demo behind the mock brain; " +
  "run `npm run e2e:hive` (which sets PW_HIVE=1 and PW_LIVE_BRAIN=1).";

/**
 * Whether this run is the **visual lane**: the same default-feature host every
 * ordinary run drives, but running `visual.spec.ts`, which compares full-page
 * renders of the console's top-level surfaces against committed baselines.
 *
 * Set `PW_VISUAL=1`, or run `npm run e2e:visual`.
 *
 * ## Why a lane rather than screenshots inside the specs that already exist
 *
 * Because a screenshot answers a different question, and answering it in the
 * wrong place would degrade the specs that already work. `shell-two-layer.spec.ts`
 * says outright why it asserts geometry instead of pixels: a sliver of inset
 * over a flat tint is structurally a two-layer shell and visually nothing, and
 * only a named quantity — eight pixels on all four sides, a fill measurably
 * different from the chrome — fails that. Every such assertion in this suite is
 * a claim someone can read, and none of them should become "it looks like it
 * did last week".
 *
 * What a baseline catches is the complement: the regression nobody had a
 * quantity for. A token that changed lightness everywhere, a font that stopped
 * loading and fell back, a card that lost its padding on one view out of nine.
 * No geometric assertion names those because nobody knew to write one, and they
 * are exactly what a reviewer notices in a screenshot within a second.
 *
 * ## Why it is not a required check
 *
 * Baselines are per-platform — Playwright suffixes them with the platform name,
 * and the ones committed here were generated on `linux`. A contributor on macOS
 * regenerates their own on first run; a required check that fails for everyone
 * who is not on the recording platform teaches people to pass `-u` reflexively,
 * which is how a baseline suite stops meaning anything. It is a tool for
 * looking at a change, not a gate on merging one.
 *
 * Run it before and after a styling change and read the diff Playwright writes
 * into `playwright-report/`. Re-record with `npm run e2e:visual:update` — and
 * read what you re-recorded, because accepting a diff is the whole risk here.
 */
export const VISUAL = process.env.PW_VISUAL === "1";

/** The reason string a `VISUAL` skip carries, so no skip is ever bare. */
export const VISUAL_REASON =
  "compares committed per-platform screenshot baselines; run `npm run e2e:visual` " +
  "(which sets PW_VISUAL=1), and `npm run e2e:visual:update` to re-record.";
