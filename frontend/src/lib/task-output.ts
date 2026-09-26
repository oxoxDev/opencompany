// The card's link to what it produced (issue #339, epic #183 §6).
//
// A task that finished used to end as a chat message: prose in a conversation,
// with no durable thing to open, share, or hand to someone else. The host now
// stamps every successful attempt with a `TaskOutput`; this module turns that
// record into the one link a card shows, and into the addresses that link
// resolves to.
//
// # The precedence lives here and nowhere else
//
// A card shows **one** primary link — artifact, else workflow, else the run
// trace — and reaches the rest through the task detail. That primary is
// *derived* from the stamp on every render rather than persisted alongside it,
// so it can never contradict the list it came from. This module is the single
// site of that rule; the host deliberately stores the facts and not the choice.
//
// # Why the routes are query strings and not path segments
//
// `hooks/use-hash-view.ts` is the app's hash router, and its `canonicalize()`
// runs on **every** `hashchange`, rewriting the URL to at most `head/sub` — two
// segments. A third segment (`#/tasks/t-1/artifacts/a-1/3`) is therefore
// silently replaced away before anything can read it, which would make these
// links appear to work and then not.
//
// `readSegments()` strips everything after `?` before that comparison, and
// `canonicalize` early-returns when the two-segment path already matches — so a
// hash **query string** survives untouched. That is load-bearing, not a style
// choice: do not "tidy" these into path segments.

import type { Task, TaskOutput } from "@/api/tasks";

/** Where a card's link points, and what to call it. */
export interface TaskLink {
  /** What is at the other end — drives the icon and the label's voice. */
  kind: "artifact" | "workflow" | "trace" | "card";
  /** The `#/…` address, ready for an anchor's `href`. */
  href: string;
  /** The operator-facing label. */
  label: string;
  /** A longer explanation for the anchor's `title`, when one helps. */
  hint?: string;
}

/** `#/tasks/<id>` — the card itself, and the link of last resort. */
export function cardHref(taskId: string): string {
  return `#/tasks/${encodeURIComponent(taskId)}`;
}

/**
 * `#/tasks/<id>?artifact=<artifactId>&v=<version>` — the task's Artifacts tab,
 * with that deliverable open at the revision the run wrote.
 */
export function artifactHref(
  taskId: string,
  artifactId: string,
  version: number,
): string {
  return `${cardHref(taskId)}?artifact=${encodeURIComponent(artifactId)}&v=${version}`;
}

/**
 * `#/artifacts/<artifactId>?v=<n>` — a published deliverable on its own page.
 *
 * Distinct from [`artifactHref`] on purpose. That one opens the **card's**
 * Artifacts tab, which is right where the card is the subject: the board's
 * primary link, and a workflow run's file list, both of which are looking at a
 * task and want its outputs in place.
 *
 * A chat row is not looking at a task. `publish_artifact` mints a card only
 * because an `ArtifactRecord`'s identity is `(task_id, source)` and the store
 * will not take an artifact without one (`ports/artifacts.rs`) — a storage
 * requirement, not a piece of work. Sending the reader through it made them
 * open a board item to read the thing the row was already offering.
 */
export function artifactPageHref(artifactId: string, version: number): string {
  return `#/artifacts/${encodeURIComponent(artifactId)}?v=${version}`;
}

/**
 * `#/tasks/<id>?run=<runId>` — the task's Attempts tab, with that attempt's
 * trace open. This is what "no artifact" resolves to, which is why it is a
 * first-class address rather than a fallback with no URL.
 */
export function traceHref(taskId: string, runId: string): string {
  return `${cardHref(taskId)}?run=${encodeURIComponent(runId)}`;
}

/**
 * `#/workflows/<id>`, plus `?run=<runId>` when the attempt actually executed
 * it — the canvas, showing what ran rather than only what the graph says now.
 */
export function workflowHref(workflowId: string, runId?: string): string {
  const base = `#/workflows/${encodeURIComponent(workflowId)}`;
  return runId ? `${base}?run=${encodeURIComponent(runId)}` : base;
}

/** Everything the stamp points at, in precedence order. */
function linksFor(taskId: string, output: TaskOutput): TaskLink[] {
  const links: TaskLink[] = [];
  for (const artifact of output.artifacts ?? []) {
    links.push({
      kind: "artifact",
      href: artifactHref(taskId, artifact.artifactId, artifact.version),
      label: `Open ${artifact.title}`,
      hint: `Opens v${artifact.version} — the version this run produced.`,
    });
  }
  for (const workflow of output.workflows ?? []) {
    links.push({
      kind: "workflow",
      href: workflowHref(workflow.workflowId, workflow.runId),
      label: `Open automation ${workflow.workflowId}`,
      hint:
        workflow.action === "ran"
          ? "Opens the automation on its canvas, showing this run."
          : "Opens the automation this task built. It has not been run yet.",
    });
  }
  // Always last, and always present: the producer is the deliverable when
  // nothing else is, and the fallback when a published artifact is later
  // deleted. This is what stops "no artifact" degrading into "no link".
  //
  // Which producer is a closed set (issue #806). A run has a trace to open; an
  // operator chat turn has no run row and never gets a synthetic one, so it
  // falls back to the card and says why. Labelling a conversation "attempt 1"
  // would be the lie the union exists to prevent.
  if ("runId" in output) {
    links.push({
      kind: "trace",
      href: traceHref(taskId, output.runId),
      label: output.attempt
        ? `View run trace · attempt ${output.attempt}`
        : "View run trace",
      hint: "Opens what this attempt actually did, step by step.",
    });
  } else {
    // The card, not the chat channel: the card's own origin row resolves the
    // thread to a Room address and offers the jump only when one exists
    // (issue #246), so pointing here cannot mint a link that lands nowhere.
    links.push({
      kind: "card",
      href: cardHref(taskId),
      label: "Open this task",
      hint: "This was settled by a chat turn rather than a run, so there is no attempt to open — the conversation it came from is linked on the card.",
    });
  }
  return links;
}

/**
 * The single link a card shows: artifact, else workflow, else the trace.
 *
 * A card with no stamp — never succeeded, dragged to Done by hand, or settled
 * before #339 — falls back to the card itself. That is deliberate and it is
 * where the epic's *"every card in Done has a link"* honestly stops: nothing
 * recorded an attempt for those, and synthesizing one would be a lie about
 * identity. A link to the card is at least true.
 */
export function primaryLink(task: Task): TaskLink {
  if (!task.output) {
    return {
      kind: "card",
      href: cardHref(task.id),
      label: "Open this task",
      hint: "This card recorded no attempt, so there is nothing else to open.",
    };
  }
  return linksFor(task.id, task.output)[0];
}

/**
 * How many further things this card produced beyond the primary link.
 *
 * The trace is not counted: it is always reachable and always last, so
 * counting it would put a `+1 more` on every single stamped card and mean
 * nothing. This counts only additional *deliverables*.
 */
export function extraOutputCount(task: Task): number {
  const output = task.output;
  if (!output) return 0;
  const deliverables =
    (output.artifacts?.length ?? 0) + (output.workflows?.length ?? 0);
  return Math.max(0, deliverables - 1);
}

/** What a `#/tasks/<id>?…` address asks the detail screen to open. */
export interface TaskFocus {
  /** An explicitly addressed always-visible task-detail tab. */
  tab?: TaskTab;
  /** Open this artifact on the Artifacts tab… */
  artifactId?: string;
  /** …pinned at this revision, when the address named one. */
  version?: number;
  /** Or open this attempt's trace on the Attempts tab. */
  runId?: string;
}

/**
 * The task-detail tabs that can be written into an address.
 *
 * Four of the five are on every card. **`plan` is not** — the Plan tab renders
 * only for a card somebody planned (issue #337) — and it is in this list all
 * the same, because the alternative is the state it was in: selecting Plan
 * wrote nothing to the URL, so a reload, a copied link or a Back/Forward
 * dropped the operator onto Timeline while the tab they were reading was the
 * only reason they were on the screen. An address that cannot name a tab is an
 * address that silently disagrees with what is on screen.
 *
 * The cost of admitting it is a `?tab=plan` that names a tab a *particular*
 * card has not got — a link that has gone stale, or one card's link opened on
 * another. `TaskDetailView` resolves that the way {@link readTaskFocus} handles
 * every other stale query: it falls back to Timeline rather than rendering an
 * empty screen. That is a screen-level fallback and not one this list can make,
 * because whether the tab exists is a property of the card, not of the address.
 */
export const TASK_TABS = ["timeline", "attempts", "plan", "artifacts", "discussion"] as const;

export type TaskTab = (typeof TASK_TABS)[number];

/**
 * Whether a query value names an addressable task-detail tab.
 *
 * Addressable, not always-present: see {@link TASK_TABS} on `plan`.
 */
export function isTaskTab(value: string): value is TaskTab {
  return (TASK_TABS as readonly string[]).includes(value);
}

/**
 * Reads the focus out of a `#/tasks/<id>?…` hash.
 *
 * Tolerant by construction: a malformed or unknown query yields an empty focus
 * and the detail screen opens on its default tab. A link that has gone stale
 * should land somewhere sensible, never on an error.
 */
export function readTaskFocus(hash: string): TaskFocus {
  const query = hash.split("?")[1];
  if (!query) return {};
  let params: URLSearchParams;
  try {
    params = new URLSearchParams(query);
  } catch {
    return {};
  }
  const focus: TaskFocus = {};
  const tab = params.get("tab");
  if (tab && isTaskTab(tab)) focus.tab = tab;
  const artifactId = params.get("artifact");
  if (artifactId) {
    focus.artifactId = artifactId;
    const version = Number.parseInt(params.get("v") ?? "", 10);
    // A version that is not a positive integer is dropped rather than
    // guessed at, which lands the reader on the newest revision — the same
    // place they would have arrived with no pin at all.
    if (Number.isInteger(version) && version > 0) focus.version = version;
  }
  const runId = params.get("run");
  if (runId) focus.runId = runId;
  return focus;
}

/**
 * Replaces the addressed task-detail tab without discarding another focus or
 * the host scope carried in the same hash query.
 */
export function taskTabHref(hash: string, tab: TaskTab): string {
  const [path, query = ""] = hash.split("?");
  const params = new URLSearchParams(query);
  params.set("tab", tab);
  return `${path}?${params.toString()}`;
}

/**
 * Which tab an address asks the task detail to open (issue #339).
 *
 * `timeline` for everything else — including a focus that names nothing, which
 * is every navigation that existed before this, and every lineage hop, whose
 * plain `#/tasks/<id>` claims the default and must therefore land on it.
 */
export function tabForFocus(focus?: TaskFocus): TaskTab {
  if (focus?.tab) return focus.tab;
  if (focus?.artifactId) return "artifacts";
  if (focus?.runId) return "attempts";
  return "timeline";
}
