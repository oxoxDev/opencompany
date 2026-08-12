// The live workflow API: the console's Workflows canvas reads the company's
// saved graphs through the host's `…/workflows` routes (REST, camelCase over
// the wire) and runs one via `…/workflows/{wid}/run`. Replaces the client-side
// `workflow-sample` illustrative data.

import type { OpenCompanyClient } from "./client";

/** A one-line workflow entry, as the picker lists it. */
export interface WorkflowSummary {
  id: string;
  name: string;
  description?: string;
  /**
   * Whether this workflow can be edited or deleted through the API (issue
   * #259). `false` for a graph defined by a file in the company source tree,
   * and for a name-only entry with no saved graph at all — the host refuses
   * both with a 409, so the console greys the affordance out instead.
   *
   * **Optional on the type, not on the wire.** A host predating #259 sends no
   * such field, and `undefined` must not read as "not editable" — treat only an
   * explicit `false` as a refusal.
   */
  editable?: boolean;
  /**
   * Whether this workflow's schedule is armed (issue #276). `false` means the
   * graph is saved and still runnable by hand, but the scheduler skips it.
   *
   * Independent of {@link WorkflowSummary.editable}: a seed-defined workflow is
   * `editable: false` and still toggleable, because pausing writes to the
   * company record rather than to the source tree.
   *
   * **Optional on the type, not on the wire**, same rule as `editable`: a host
   * predating #276 sends no such field, and `undefined` must not render as
   * paused — treat only an explicit `false` as off.
   */
  enabled?: boolean;
}

/** A single graph node. `kind` is one of the tinyflows node kinds. */
export interface WorkflowNode {
  id: string;
  /**
   * `trigger` | `agent` | `tool_call` | `http_request` | `condition` |
   * `output` | `switch` | `merge` | `split_out` | `transform` |
   * `output_parser` | `sub_workflow`.
   */
  kind: string;
  name: string;
  summary?: string;
  /** The roster agent id — only present on `agent` nodes. */
  agent?: string;
  /**
   * A standard 5-field cron saying when the workflow starts on its own — only
   * present on `trigger` nodes, and always interpreted in **UTC** (issue #169).
   * Absent means the workflow only runs when something starts it (the Run
   * button, the run route, or another workflow).
   */
  schedule?: string;
  /** Kind-specific configuration (a slug, URL, case labels, schema, …). */
  config?: unknown;
  /** How the engine handles an error on this node, when set. */
  onError?: string;
  /** The node's retry policy, when set. */
  retry?: {
    maxAttempts?: number;
    backoffMs?: number;
    backoff?: string;
  };
  /** Whether the node pauses for a human approval before proceeding. */
  requiresApproval?: boolean;
  /** Where an `output` node's report goes when the run finishes. */
  destination?: WorkflowDestination;
}

/**
 * Where a terminal `output` node routes its report once the run completes.
 *
 * `owner` is resolved server-side from the company's admins and carries no
 * target — the graph names nobody, which is what keeps it safe by construction.
 * `email` names an address and only sends when the company grants `email` AND
 * the recipient has already written in. `channel` must name a channel the
 * deployment already wired.
 */
export interface WorkflowDestination {
  kind: "owner" | "email" | "channel";
  /** Required for `email` (an address) and `channel` (an id); absent for `owner`. */
  target?: string;
}

/**
 * The destination kinds the creator's picker offers, with prosumer labels.
 *
 * Kept in step with the host's `WORKFLOW_DESTINATION_KINDS` by
 * `destination_kinds_match_the_console` in `src/company/workflow_file.rs`, which
 * reads the `value:` entries out of this very block — so adding a kind on one
 * side alone fails `cargo test` rather than shipping a picker the server rejects
 * (or withholding one it accepts). Issue #260.
 */
export const DESTINATION_KINDS: { value: WorkflowDestination["kind"]; label: string }[] = [
  { value: "owner", label: "Owner — the company's admins" },
  { value: "email", label: "Email — a specific address" },
  { value: "channel", label: "Channel — a wired chat channel" },
];

/** A directed edge between two node ids, with an optional branch label. */
export interface WorkflowEdge {
  from: string;
  to: string;
  label?: string;
}

/** The full graph the canvas renders. */
export interface WorkflowGraph {
  id: string;
  name: string;
  description?: string;
  nodes: WorkflowNode[];
  edges: WorkflowEdge[];
  /** See {@link WorkflowSummary.editable}. Same "only `false` means no" rule. */
  editable?: boolean;
  /** See {@link WorkflowSummary.enabled}. Same "only `false` means off" rule. */
  enabled?: boolean;
  /**
   * The opaque optimistic-concurrency token for this graph (issue #259),
   * present only when `editable`.
   *
   * **Echo it back, never parse it.** Pass it to {@link updateWorkflow} or
   * {@link deleteWorkflow} and the host refuses the write with a 409 if the
   * graph changed since this read — which is what stops one console silently
   * overwriting another's edit. Absent from a host predating #259, in which case
   * the write is unconditional and that protection simply does not exist.
   */
  version?: string;
}

/**
 * What became of one attempt to deliver an output node's report.
 *
 * `pending` means the send was parked for operator approval (a cold email
 * recipient a workflow may not open a conversation with on its own). It is a
 * SNAPSHOT taken when the run finished: runs are not persisted, so nothing ever
 * comes back to flip the row to `sent`. The Approvals view is the live source of
 * truth — approving there actually sends the mail.
 */
export type DeliveryStatus = "sent" | "pending" | "skipped" | "denied" | "failed";

/**
 * One attempt to route a reached `output` node's report to its destination.
 *
 * This is the ONLY place an operator learns a report was not delivered: a
 * delivery failure never fails the run, so it has nowhere else to surface. An
 * output node the run never reached contributes no row at all.
 */
export interface DeliveryReport {
  /** The output node whose report this was. */
  node: string;
  /** The destination kind as authored. */
  kind: string;
  /** The address or channel actually addressed, when there was one. */
  target?: string;
  status: DeliveryStatus;
  /**
   * An operator-readable reason — populated even on success. This is the half
   * the drawer renders: it may quote the transport verbatim, which is what
   * makes a refused send fixable, and this console is a tenant surface.
   */
  detail: string;
  /**
   * The same outcome as a stable token out of a closed set (issue #248) —
   * `"mail-transport-refused"`, `"channel-not-wired"`, and so on.
   *
   * It exists because the host's own logs may not carry `detail`: on the
   * transport-failure arms `detail` interpolates the transport's reply, which
   * routinely quotes the recipient's address, and host stdout is a platform
   * surface rather than a tenant one. Nothing renders it today — `detail` is
   * strictly more informative here — so it is optional on the type (not the
   * wire), and a response from a host predating #248 still parses.
   */
  reason?: string;
}

/** The result of a run: the engine's final state and any pending approvals. */
export interface WorkflowRunResult {
  /** The engine's final run state — a nested JSON payload. */
  output: unknown;
  /** Node ids left waiting on a human approval, if any. */
  pendingApprovals: string[];
  /**
   * One row per report-delivery attempt. Optional on the type (not the wire)
   * so a response from a host predating issue #170 still parses.
   */
  deliveries?: DeliveryReport[];
  /**
   * The run's correlation id (issue #371). Optional on the type so a response
   * from a host predating #371 still parses.
   *
   * The console needs it because the run's progress events arrive over SSE
   * *while this request is still in flight*: without it, a cron fire that
   * overlapped the manual run could not be told apart from the run being
   * awaited, and its frames would repaint the canvas.
   */
  runId?: string;
  /**
   * Per-node progress for this run, in the order the nodes finished (issue
   * #542) — the same structural rows {@link WorkflowRunOutcome.nodes} carries.
   * Present on every synchronous run from a host that supports it; for a dry
   * run it is the ONLY record of what ran, since a test run journals nothing.
   * Optional on the type (not the wire) so a response from an older host still
   * parses.
   */
  nodes?: WorkflowRunNode[];
  /**
   * `true` when the host ran this as a **dry run / test run** (issue #542): the
   * real graph walked over stubbed effects, so nothing was sent, no tokens were
   * spent, and nothing was journaled or parked.
   *
   * **The presence discriminator, and always `true` when set.** An older host
   * ignores the `dryRun` request flag and runs FOR REAL, answering with a body
   * that has no `dryRun` key — so a console that asked for a test run must read
   * this back (see {@link isDryRun}) rather than trust what it asked for, and
   * warn loudly when it is absent. Optional/absent must read as "was NOT a dry
   * run", i.e. the run was real.
   */
  dryRun?: boolean;
}

/**
 * The `detach: true` answer (issue #383): the run's id, before it has finished.
 *
 * `detached` is the discriminator and it is always `true`. See
 * {@link isDetached} for why the shape rather than the request is what the
 * console must read.
 */
export interface WorkflowRunAccepted {
  runId: string;
  detached: true;
}

/**
 * What `POST …/workflows/{wid}/run` can answer with — a settled run, or an
 * accepted one.
 */
export type WorkflowRunResponse = WorkflowRunResult | WorkflowRunAccepted;

/**
 * Whether the host answered "accepted, watch the stream" rather than "here is
 * the finished run".
 *
 * **Discriminates on the response, never on what we asked for**, and that is
 * the whole compatibility story. A host predating #383 ignores the unknown
 * `detach` field — the body has no `deny_unknown_fields` — and answers the full
 * synchronous `200`. So a console that assumed "I asked to detach, therefore
 * this is a run id" would read a settled run's body as an acceptance and then
 * wait forever for frames that already arrived.
 *
 * Reading `detached` back is also what makes the older-host case *work* rather
 * than merely not crash: the settled body is a perfectly good answer, so the
 * console simply uses it.
 */
export function isDetached(response: WorkflowRunResponse): response is WorkflowRunAccepted {
  return (response as WorkflowRunAccepted).detached === true;
}

/**
 * Whether the host actually ran this as a **dry run** (issue #542).
 *
 * **Discriminates on the response, never on what we asked for** — the same
 * compatibility story as {@link isDetached}. A host predating test mode ignores
 * the `dryRun` request flag and runs FOR REAL, and its settled body carries no
 * `dryRun` key. So a console that asked for a test run and reasoned "I asked to
 * dry-run, therefore nothing happened" would be wrong on exactly the hosts where
 * a real run just fired every effect. Read `dryRun` back instead, and when it is
 * absent from a run you asked to be dry, warn loudly: the run was real.
 *
 * Only meaningful on a settled result (a detached response carries no output and
 * no `dryRun`); guarded so it is safe to call on either shape.
 */
export function isDryRun(response: WorkflowRunResponse): boolean {
  return (response as WorkflowRunResult).dryRun === true;
}

/** One node's outcome inside a run (issue #371). */
export interface WorkflowRunNode {
  nodeId: string;
  /** Whether the node succeeded or errored. */
  status: "ok" | "error";
  /** Wall-clock duration of the node's execution, in milliseconds. */
  elapsedMs: number;
}

/**
 * One finished workflow run, read back from the company's journal (issue #228).
 *
 * Before this, a run's outcome existed only in the moment: a manual run's
 * `deliveries` lived in the run drawer until it was dismissed, and a scheduled
 * run's reached only the host's stdout — which on a hosted tenant is the
 * platform team, not the operator. This is the same information, durable, so it
 * survives a console reload and a scheduled run nobody watched.
 */
export interface WorkflowRunOutcome {
  /** The journal sequence position — a stable, monotonic row key. */
  seq: number;
  /** Epoch-millis the outcome was recorded. */
  atMillis: number;
  workflowId: string;
  /** True when a cron started the run rather than an operator. */
  scheduled: boolean;
  /** A run correlation id, when the entry point minted one. */
  runId?: string;
  /** The same delivery rows a manual run's response carries. */
  deliveries: DeliveryReport[];
  /** Node ids the run left waiting on a human approval. */
  pendingApprovals: string[];
  /** Set when the run failed outright instead of finishing with rows. */
  error?: string;
  /**
   * Per-node progress, in the order the nodes finished (issue #371). Absent on
   * a run journaled before #371 — so an empty/absent list means "no per-node
   * trail", never "the run did nothing".
   */
  nodes?: WorkflowRunNode[];
  /** When the run started. Absent on a pre-#371 row, whose only time is the finish. */
  startedAtMillis?: number;
  /**
   * True for a run that started and has not settled.
   *
   * Trustworthy only because the host settles runs its previous process left
   * open, at boot — so nothing sits here spinning forever.
   */
  running?: boolean;
  /**
   * True for a run an operator stopped (issue #383).
   *
   * Separate from {@link error} because it is a separate outcome: a cancelled
   * run carries no error, so reading only `error` would render a deliberate
   * stop as a clean success. With `error` this gives the three terminal
   * readings the history distinguishes — failed, interrupted by a host restart,
   * stopped by an operator.
   *
   * Optional on the type, not the wire: a host predating #383 sends nothing,
   * and absent must read as "not cancelled".
   */
  cancelled?: boolean;
  /**
   * System notices raised about this run (issue #638) — today, that a node
   * gated more tool calls than the per-batch cap allows and the excess was
   * discarded.
   *
   * NOT a failure. A run that overflowed the cap still succeeded: its nodes
   * ran and its output is valid. Render it as a warning beside the outcome,
   * never as an error, or a run that worked reads as one that broke.
   *
   * Omitted on the wire for the overwhelming majority of runs, which raise
   * nothing — so absent must read as "nothing to tell you", not "unknown".
   */
  notices?: string[];
}

export function listWorkflows(
  client: OpenCompanyClient,
  company: string | null,
): Promise<WorkflowSummary[]> {
  return client.get<WorkflowSummary[]>(`${client.scopeFor(company)}/workflows`);
}

export function getWorkflow(
  client: OpenCompanyClient,
  company: string | null,
  wid: string,
): Promise<WorkflowGraph> {
  return client.get<WorkflowGraph>(
    `${client.scopeFor(company)}/workflows/${encodeURIComponent(wid)}`,
  );
}

/** The `GET …/workflows/tool-slugs` answer (issue #783). */
interface WorkflowToolSlugsResponse {
  slugs: string[];
}

/**
 * The wired, granted `tool_call` slugs the per-workflow copilot may ground a
 * proposal on (issue #783).
 *
 * Every slug here is one a proposed `tool_call` node would clear at the host's
 * courtesy validation — the route serves the SAME set the create-time copilot
 * grounds on (issue #753), so what the copilot is told it can call and what the
 * host will accept cannot drift. A host predating the route 404s; the caller
 * degrades to an empty list, exactly as it does for the roster read, rather than
 * blocking the copilot.
 */
export function listWorkflowToolSlugs(
  client: OpenCompanyClient,
  company: string | null,
): Promise<string[]> {
  return client
    .get<WorkflowToolSlugsResponse>(`${client.scopeFor(company)}/workflows/tool-slugs`)
    .then((r) => r.slugs);
}

/**
 * Runs a workflow.
 *
 * **The console omits `detach` on purpose (issue #528).** A run's full `output`
 * is carried ONLY by the settled `200` body: the journal, the SSE frames, and
 * {@link listWorkflowRuns} are all structural (per-node status and timing, no
 * agent text), and there is no run-detail route to fetch the output back later.
 * The one surface that renders what a run produced mounts on that body, so the
 * console must hold the request open to receive it. This is affordable because
 * since #383 the host runs even the synchronous path on a spawned server task —
 * the run itself survives a dropped connection; only the answer rides the wire.
 *
 * With `detach` the host instead answers as soon as the run has an id (`202`),
 * without holding the request open — which stops a proxy's idle timeout from
 * severing a healthy multi-minute run, at the cost of the console never seeing
 * the output. The option is kept for callers that only need the run to start
 * (e.g. a fire-and-forget trigger) and will read the outcome from the stream or
 * the history.
 *
 * **Always check {@link isDetached} on the result**, whatever you asked for: a
 * host predating #383 ignores the flag and answers with the settled run, and
 * that answer is fine to use.
 *
 * **`dryRun` (issue #542)** asks the host to run a **test run**: the real graph
 * over stubbed effects, so nothing is sent and no tokens are spent. It is
 * synchronous by nature — the whole point is to read the settled `output` and
 * per-node `nodes` back — so it composes with the default (no `detach`). Just
 * like `detach`, an older host ignores the flag and runs FOR REAL, so the caller
 * MUST confirm with {@link isDryRun} rather than assume, and warn when a run it
 * asked to be dry comes back without the marker.
 */
export function runWorkflow(
  client: OpenCompanyClient,
  company: string | null,
  wid: string,
  input?: unknown,
  options?: { detach?: boolean; dryRun?: boolean },
): Promise<WorkflowRunResponse> {
  return client.post<WorkflowRunResponse>(
    `${client.scopeFor(company)}/workflows/${encodeURIComponent(wid)}/run`,
    {
      input: input ?? {},
      ...(options?.detach ? { detach: true } : {}),
      ...(options?.dryRun ? { dry_run: true } : {}),
    },
  );
}

/**
 * Stops a run that is still walking its graph (issue #383).
 *
 * Resolves when the host has fired the stop signal — **not** when the run has
 * wound down. The run settles a moment later and announces itself on the event
 * stream with `cancelled`, which is the frame to believe.
 *
 * Throws `404` when the run is unknown or has already settled (one answer:
 * there is nothing to stop) and when the host predates this route. Callers
 * should tell those apart by context rather than by the status, and degrade to
 * "this host can't stop runs" rather than surfacing a raw error.
 *
 * Stopping is not finishing: the executing node is dropped mid-flight, so an
 * external side effect it had started may be half-done. Completed nodes stay in
 * the run history, and approvals earlier nodes parked stay valid in the queue.
 */
export function cancelWorkflowRun(
  client: OpenCompanyClient,
  company: string | null,
  runId: string,
): Promise<{ cancelling: boolean }> {
  return client.post<{ cancelling: boolean }>(
    `${client.scopeFor(company)}/workflows/runs/${encodeURIComponent(runId)}/cancel`,
    {},
  );
}

/**
 * The company's finished workflow runs, **newest first** (issue #228).
 *
 * `workflow` narrows to one graph's runs; `limit` caps the page (the host
 * defaults to a short recent list and clamps a large ask). A host predating this
 * route answers 404 — callers should treat that as "no history yet" rather than
 * an error, since the console still works without it.
 */
export function listWorkflowRuns(
  client: OpenCompanyClient,
  company: string | null,
  options?: { workflow?: string; limit?: number },
): Promise<WorkflowRunOutcome[]> {
  const params = new URLSearchParams();
  if (options?.workflow) params.set("workflow", options.workflow);
  if (options?.limit) params.set("limit", String(options.limit));
  const query = params.toString();
  return client.get<WorkflowRunOutcome[]>(
    `${client.scopeFor(company)}/workflows/runs${query ? `?${query}` : ""}`,
  );
}

/**
 * One past run's durable per-node output snapshot (issue #596) — the data the
 * run inspector renders when an operator opens a node.
 *
 * `nodes` is the engine's `{ "<node id>": { "items": [ … ] } }` map, bounded for
 * storage; `truncated` says whether any value was clipped to fit the caps, so the
 * inspector can badge it honestly.
 */
export interface WorkflowRunOutputRecord {
  runId: string;
  workflowId: string;
  /** Epoch-millis the snapshot was captured (the run's settle time). */
  atMillis: number;
  /** The per-node output map — `{ "<node id>": { "items": [ … ] } }`. */
  nodes: unknown;
  /** Whether any value was clipped to fit the durable size caps. */
  truncated: boolean;
}

/**
 * Fetches one past run's per-node output snapshot (issue #596).
 *
 * This is the durable extension of {@link runWorkflow}'s in-memory `output`: a
 * run reopened from History has no live result, so the inspector reads what each
 * node produced from here instead. Lazy per-run by design — the history list
 * stays structural (status + timing), and only the run an operator clicks into is
 * fetched.
 *
 * **Throws `404` for a run with no captured output**, which is the normal state
 * for a run that predates this feature, a dry run (persists nothing), or a
 * hard-aborted run (no outcome to persist) — and for a host predating this
 * route. Callers should treat a 404 as "no output to show" and render the empty
 * state, not surface an error.
 */
export function workflowRunOutput(
  client: OpenCompanyClient,
  company: string | null,
  runId: string,
): Promise<WorkflowRunOutputRecord> {
  return client.get<WorkflowRunOutputRecord>(
    `${client.scopeFor(company)}/workflows/runs/${encodeURIComponent(runId)}/output`,
  );
}

/**
 * Authors a new workflow graph (issues #69, #168): the console's form creator
 * posts the same shape `getWorkflow` returns, and the host persists it on the
 * company record — so this works on every deployment, including a hosted tenant
 * whose company source tree is a read-only mount. Rejections carry a
 * prosumer-language `ApiError` message (bad id, duplicate id or name, an edge or
 * `agent` node the graph can't support).
 */
export function createWorkflow(
  client: OpenCompanyClient,
  company: string | null,
  graph: WorkflowGraph,
): Promise<WorkflowGraph> {
  return client.post<WorkflowGraph>(`${client.scopeFor(company)}/workflows`, graph);
}

/**
 * What the create-time copilot drafted from an operator's description (issue
 * #753).
 *
 * Exactly one shape is returned, told apart by `automatable`: a drafted
 * `workflow` to review, or a `reason` the described work is better done once.
 * The host answers **200 in both cases** (like {@link previewCron}) — neither is
 * an error the operator fixes by retrying differently.
 *
 * `workflow` is a plain {@link WorkflowGraph}: the host strips the model's
 * approval gating and mints the id, and the node/edge shape is the same camelCase
 * the read routes return, so it hydrates the create form with no adapter. It
 * carries no `version` (nothing is saved yet) — Create persists it for the first
 * time.
 */
export interface WorkflowDraftFromDescription {
  /** `true` with a `workflow` to review; `false` with a `reason`. */
  automatable: boolean;
  /** A one-line gloss of what the drafted workflow does. Present when `automatable`. */
  summary?: string;
  /** The drafted graph to hydrate the create form. Present when `automatable`. */
  workflow?: WorkflowGraph;
  /** Why the work is better done once. Present when not `automatable`. */
  reason?: string;
}

/**
 * Drafts a workflow graph from a free-text description (issue #753) — the
 * New-workflow dialog's copilot.
 *
 * **Nothing is persisted.** The host validates the draft the same way Create
 * would, hands it back, and the operator reviews it in the hydrated form before
 * pressing Create (which is the only call that saves). So this is safe to call
 * speculatively; a bad draft costs a review, not a rollback.
 *
 * A build with no embedded brain answers a capability gap the same way the run
 * route does — `not_wired` (404), `restart_required` or `inference_required`
 * (409) — which the caller should surface inline rather than treat as a drafted
 * answer. An empty description is a `400`.
 */
export function draftWorkflowFromDescription(
  client: OpenCompanyClient,
  company: string | null,
  description: string,
): Promise<WorkflowDraftFromDescription> {
  return client.post<WorkflowDraftFromDescription>(
    `${client.scopeFor(company)}/workflows/draft-from-description`,
    { description },
  );
}

/**
 * Replaces a saved workflow graph wholesale (issue #259) — the fix for a
 * workflow being write-once, so a typo'd cron or a node pointed at the wrong
 * teammate can be corrected instead of abandoned.
 *
 * `graph.id` must equal `wid`: a workflow's id keys its saved graph, its
 * schedule and its run history, so the host rejects a rename with a `400`. A
 * rename is a create plus a delete.
 *
 * Pass `expectedVersion` — the `version` from the {@link getWorkflow} this edit
 * was based on — to make the write conditional. If the graph moved in between,
 * the host answers `409` and **nothing is written**; surface that to the
 * operator with a reload rather than retrying without the token, which is the
 * silent-overwrite the guard exists to prevent.
 *
 * Other rejections carry the same prosumer-language `ApiError` a create does:
 * `400` for a bad graph, `404` for an unknown id, `409` for a source-defined
 * workflow or a display name already taken.
 *
 * Returns the stored graph with a **fresh** `version`, so a second save needs no
 * intervening read.
 */
export function updateWorkflow(
  client: OpenCompanyClient,
  company: string | null,
  wid: string,
  graph: WorkflowGraph,
  expectedVersion?: string,
): Promise<WorkflowGraph> {
  return client.put<WorkflowGraph>(
    `${client.scopeFor(company)}/workflows/${encodeURIComponent(wid)}`,
    expectedVersion ? { ...graph, expectedVersion } : graph,
  );
}

/**
 * Removes a saved workflow (issue #259): the graph body and its entry in the
 * company's enabled list, in one write, so it leaves the picker and stops
 * firing on its schedule — and stays gone across a host restart.
 *
 * **Past runs are kept.** They record what the workflow did, which stays true
 * after it is gone; {@link listWorkflowRuns} keeps serving them.
 *
 * `expectedVersion` makes the delete conditional in exactly the sense the
 * operator means by clicking Delete on a graph they are looking at: if it
 * changed underneath them, the host answers `409` and removes nothing.
 *
 * Follows the same runtime-vs-source contract as `deleteDesk`: a workflow
 * defined by a file in the company source tree cannot be removed from the
 * console and returns `409`; an unknown id is `404`.
 */
export function deleteWorkflow(
  client: OpenCompanyClient,
  company: string | null,
  wid: string,
  expectedVersion?: string,
): Promise<void> {
  const query = expectedVersion
    ? `?expectedVersion=${encodeURIComponent(expectedVersion)}`
    : "";
  return client.del<void>(
    `${client.scopeFor(company)}/workflows/${encodeURIComponent(wid)}${query}`,
  );
}

/**
 * Arms or pauses a workflow's schedule without touching its graph (issue #276).
 *
 * **Pausing stops the schedule, not the workflow.** A paused workflow keeps its
 * graph, stays in the picker, and still runs from the Run button — only the
 * scheduler consults the flag. Resolves with the workflow's graph as the host
 * now holds it, so a caller renders the state the store agreed to rather than
 * the one it asked for.
 *
 * Unlike {@link updateWorkflow} and {@link deleteWorkflow} this takes **no**
 * `expectedVersion`: a switch has no content to overwrite, and requiring a token
 * would make a seed-defined workflow untoggleable since only overlay bodies have
 * one. It is also a wider set than those two — a `409` here means only "this id
 * has no saved graph, so there is no schedule to switch off"; an unknown id is
 * `404`.
 *
 * Idempotent: setting the state a workflow already holds is a `200` that changes
 * nothing.
 */
export function setWorkflowEnabled(
  client: OpenCompanyClient,
  company: string | null,
  wid: string,
  enabled: boolean,
): Promise<WorkflowGraph> {
  return client.put<WorkflowGraph>(
    `${client.scopeFor(company)}/workflows/${encodeURIComponent(wid)}/enabled`,
    { enabled },
  );
}

/**
 * One entry in a workflow's edit history (issue #274) — **metadata only**.
 *
 * The graph body is deliberately absent: the history list is a chooser, and the
 * body arrives (and is applied to the canvas) only when
 * {@link restoreWorkflowRevision} actually restores one. `version` is the same
 * opaque token {@link getWorkflow} hands out, so the console can tell which
 * snapshot matches the graph it currently holds.
 */
export interface WorkflowRevision {
  /** Stable id of the snapshot, used to address it for a restore. */
  id: string;
  /** The workflow's display name at the moment the snapshot was captured. */
  name: string;
  /** The opaque version token of the snapshotted body. Never parse it. */
  version: string;
  /** Epoch-millis the snapshot was captured. */
  createdAtMillis: number;
}

/** The `GET …/workflows/{wid}/revisions` response. */
interface WorkflowRevisionsResponse {
  revisions: WorkflowRevision[];
}

/**
 * Lists one workflow's edit history (issue #274), **newest first**.
 *
 * Each `PUT` that actually changed the graph left the prior body here, bounded
 * to the most recent 20. A workflow that was never edited — or a seed-defined
 * one that cannot be edited from the console — resolves to an empty list rather
 * than an error: "no history" is a normal state the History panel renders as
 * empty, not a failure.
 *
 * Returns metadata only (see {@link WorkflowRevision}); the graph body is
 * fetched by {@link restoreWorkflowRevision} when an operator picks one.
 */
export async function listWorkflowRevisions(
  client: OpenCompanyClient,
  company: string | null,
  wid: string,
): Promise<WorkflowRevision[]> {
  const res = await client.get<WorkflowRevisionsResponse>(
    `${client.scopeFor(company)}/workflows/${encodeURIComponent(wid)}/revisions`,
  );
  return res.revisions;
}

/**
 * Restores a workflow to one of its captured revisions (issue #274), returning
 * the restored graph with a **fresh** `version`.
 *
 * A restore is an ordinary edit whose new body is an old one, so it inherits
 * everything {@link updateWorkflow} guarantees: the revision is re-validated
 * against the *current* company (a snapshot naming a since-removed teammate is a
 * `400`, not a broken restore), the body it replaces is itself snapshotted (so a
 * restore is undoable), and a restored schedule lands **switched off** pending
 * review (issue #276) — read `enabled` on the result to reflect that.
 *
 * Pass `expectedVersion` — the `version` of the graph the operator was looking
 * at — to make the restore conditional. On a `409` the graph moved underneath
 * them: **reload and let them re-choose, do not retry** without the token, which
 * is the silent-overwrite the guard exists to prevent. Other rejections carry
 * the host's prosumer-language message: `404` for an unknown workflow or
 * revision, `409` for a source-defined / body-less workflow or a name collision.
 */
export function restoreWorkflowRevision(
  client: OpenCompanyClient,
  company: string | null,
  wid: string,
  revisionId: string,
  expectedVersion?: string,
): Promise<WorkflowGraph> {
  return client.post<WorkflowGraph>(
    `${client.scopeFor(company)}/workflows/${encodeURIComponent(wid)}/revisions/${encodeURIComponent(
      revisionId,
    )}/restore`,
    expectedVersion ? { expectedVersion } : {},
  );
}

/**
 * The node kinds the form creator's palette offers.
 *
 * The first six author from a bare form — `trigger` starts the graph, `agent`
 * runs a teammate, `condition` branches, `merge` fans inputs into one stream,
 * `transform` reshapes items, and `output` reports back. The last five need
 * kind-specific config to run (a slug, a URL, a branch key, a schema, a
 * `workflow_id`); they were withheld until the config forms landed, and now
 * carry them — each renders `NodeConfigFields`, whose spec table
 * (`@/lib/workflow-node-config`) is the single source of the engine keys it
 * emits (issue #541). Every one of these kinds also renders on the canvas and
 * can be authored by hand in `workflows/<id>.toml`.
 */
/**
 * What a trigger's cron expression actually means, per the host's parser
 * (issue #262).
 *
 * Exactly one of `error` or (`description`, `next`) is present — the host
 * answers with two shapes, not one shape with half its fields null.
 * `description` is `null` for a schedule the host declines to paraphrase; the
 * fire times still say what it means.
 */
export interface CronPreview {
  /** A plain-English gloss, or `null` when the shape is too gnarly for one. */
  description?: string | null;
  /**
   * The next few fire times as epoch millis. The UTC reading and the viewer's
   * local reading are BOTH rendered from these numbers, so the two can never
   * disagree about the same instant — which is the entire point, since the
   * schedule is UTC and the author is usually not.
   */
  next?: number[];
  /** The parser's message, when the expression did not parse. */
  error?: string;
}

/**
 * Previews a cron expression (issue #262).
 *
 * **Answers 200 even for a malformed expression**, with the parser's message in
 * `error`. The console calls this while the author is still typing, so a
 * half-written expression is the normal state — and {@link OpenCompanyClient}
 * throws on any non-2xx, so a 400 per keystroke would make an ordinary parse
 * failure arrive as a thrown error. Callers still need a `catch` for genuine
 * network failure; they should render nothing in that case rather than block
 * authoring on a preview.
 *
 * `after` pins the instant the fire times are computed from; the host defaults
 * to now.
 */
export function previewCron(
  client: OpenCompanyClient,
  company: string | null,
  expr: string,
  after?: number,
): Promise<CronPreview> {
  return client.post<CronPreview>(
    `${client.scopeFor(company)}/workflows/cron/preview`,
    { expr, after },
  );
}

export const CREATABLE_NODE_KINDS: { value: string; label: string }[] = [
  { value: "trigger", label: "Trigger — starts the workflow" },
  { value: "agent", label: "Agent — a teammate performs a step" },
  { value: "condition", label: "Condition — branches on something" },
  { value: "merge", label: "Merge — combines several inputs into one" },
  { value: "transform", label: "Transform — reshapes the data" },
  { value: "output", label: "Output — reports the result back" },
  { value: "tool_call", label: "Tool call — runs a tool by slug" },
  { value: "http_request", label: "HTTP request — calls a URL" },
  { value: "switch", label: "Switch — routes to a labeled branch" },
  { value: "output_parser", label: "Output parser — coerces to a schema" },
  { value: "sub_workflow", label: "Sub-workflow — runs another workflow" },
];

/**
 * Every node kind the host accepts in a saved graph — the OpenCompany authoring
 * contract, mirroring `WORKFLOW_NODE_KINDS` in `src/company/workflow_file.rs`.
 *
 * Broader than {@link CREATABLE_NODE_KINDS} on purpose: the palette omits
 * `split_out` (it has no create control yet), but a graph may legitimately
 * contain one, so a copilot proposal that adds one must not be refused. This is
 * the set the proposal validator checks a proposed `kind` against — a kind the
 * host would reject on write is caught here, before the operator is shown a diff
 * for a step that cannot be applied.
 */
export const WORKFLOW_NODE_KINDS: readonly string[] = [
  "trigger",
  "agent",
  "tool_call",
  "http_request",
  "condition",
  "output",
  "switch",
  "merge",
  "split_out",
  "transform",
  "output_parser",
  "sub_workflow",
];
