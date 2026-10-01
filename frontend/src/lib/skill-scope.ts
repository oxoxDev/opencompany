// The three-state skill-scope arithmetic, shared by the two surfaces that edit
// it.
//
// A teammate's page picks skills for one agent; a skill's detail panel picks
// agents for one skill. The two are transposed, so the *component* is not
// shareable — but the arithmetic is the same arithmetic in both directions, and
// it was an un-exported local function inside `AgentDetailView` until a second
// picker needed it. Two copies of this would drift, and the way they drift is
// silent: every function below returns a legal scope for an illegal reason.
//
// The state is what the agent record stores, never what resolves from it.
// `requested === null` (never edited) and `requested === []` (edited to hold
// nothing) put a slug outside the effective set identically while the company has
// the skill disabled, and only the first gets it back when the switch returns.

import type { SkillAgentScope } from "@/api/skills";

/** Which of the three stored states an agent's scope puts one skill in. */
export type SkillScopeState = "inherited" | "included" | "excluded";

/**
 * Where a stored scope puts one slug.
 *
 * The host answers this per skill on `GET …/skills`; this is the same rule on
 * the roster read's `requested`, which is what the write is computed from.
 * Matching is exact — a slug is a flat identifier, and a prefix match would
 * silently reach a skill installed after the scope was written.
 */
export function skillScopeState(
  requested: string[] | null | undefined,
  slug: string,
): SkillScopeState {
  if (requested === null || requested === undefined) return "inherited";
  return requested.includes(slug) ? "included" : "excluded";
}

/**
 * Whether the editor is still showing an inherited scope rather than a draft.
 *
 * An inherited scope stores nothing and renders every switch on, so the editor
 * needs a `touched` flag beside it: without one, the first switch turned off
 * renders on again, because the stored state still says "inherit".
 */
export function showingInherited(
  requested: string[] | null,
  touched: boolean,
): boolean {
  return requested === null && !touched;
}

/**
 * The scope after one switch moves — **the one function that can lose data**.
 *
 * An inherited scope holds every available slug while storing none, so the
 * first switch turned off has to write the rest out explicitly. Narrowing from
 * the stored list instead would save "only the ones I flicked" for a screen
 * reading "all but this one".
 *
 * Turning one **on** has the same edge in the other direction and it is the
 * sharper one, because the surface it bites is the skill panel: ticking agent B
 * on skill S must produce B's stored list *plus* S. A base of `[S]` would strip
 * every other skill B has, report success, and look right in the panel
 * afterwards — the state it left behind is a legal narrowing, so nothing refuses
 * it and nothing else on the screen disagrees.
 *
 * `available` is the ceiling the scope narrows within — the company's enabled
 * set. `draft` is an edit already in progress: pass it with its `touched` flag
 * and the move narrows that draft instead of the stored list. Omit it and the
 * move is the first one, straight off what is stored.
 *
 * `touched` cannot be inferred from the draft. An operator who turns every
 * switch off and one back on leaves an empty draft that is nothing like an
 * inherited scope, and treating it as one would hand the teammate the whole
 * ceiling.
 */
export function toggleSkillInScope(
  requested: string[] | null,
  available: string[],
  slug: string,
  on: boolean,
  draft?: { slugs: string[]; touched: boolean },
): string[] {
  const base = draft
    ? showingInherited(requested, draft.touched)
      ? available
      : draft.slugs
    : (requested ?? available);
  return on ? [...new Set([...base, slug])] : base.filter((s) => s !== slug);
}

/**
 * Whether a draft says the same thing the record already stores.
 *
 * The asymmetry is deliberate. Against a stored `null` the draft has to equal
 * the whole ceiling to be unchanged, because that is what inheriting means —
 * and saving the ceiling out as an explicit list is **not** a no-op: the
 * teammate stops inheriting, so every skill enabled afterwards passes it by.
 */
export function scopeUnchanged(
  requested: string[] | null,
  draft: string[],
  available: string[],
): boolean {
  const drafted = new Set(draft);
  return requested === null
    ? draft.length === available.length &&
        available.every((slug) => drafted.has(slug))
    : requested.length === draft.length &&
        requested.every((slug) => drafted.has(slug));
}

/**
 * Stored slugs that confer nothing, because the company does not have them.
 *
 * A scope entry the company has not enabled is stored rather than refused — the
 * picker renders against a set fetched at page load, and a concurrent uninstall
 * would otherwise fail an honest save. Saying so is the only thing between an
 * operator and a scope that quietly grants nothing.
 */
export function droppedSlugs(
  requested: string[] | null,
  effective: string[],
): string[] {
  return (requested ?? []).filter((slug) => !effective.includes(slug));
}

/**
 * Whether ticking or unticking this agent will pin a scope that currently
 * inherits.
 *
 * A one-way door from the skill panel: materializing an inherited scope fixes it
 * to today's enabled set, so a skill the company enables next month will not
 * reach that teammate. The panel says so before the save rather than after,
 * because the control that undoes it is on the teammate's own page.
 */
export function pinsAnInheritedScope(state: SkillScopeState): boolean {
  return state === "inherited";
}

/**
 * The sentence the skill panel shows before pinning an inheriting teammate.
 *
 * One string with one test on it: what must not regress is the claim, not its
 * placement.
 */
export const SCOPE_PINS_INHERITED_WARNING =
  "Removing a teammate that inherits every skill will pin its list to today's set, " +
  "so a skill you enable later will not reach it. Hand it back to inheriting on the teammate's own page.";

/**
 * The teammates "All agents" has to write, which is every one not already
 * inheriting.
 *
 * An inheriting teammate is already reached by every enabled skill, so writing
 * to it would be a no-op that could only lose a race with another editor.
 */
export function agentsToUnpin(
  agents: readonly SkillAgentScope[] | null | undefined,
): SkillAgentScope[] {
  return (agents ?? []).filter((agent) => agent.state !== "inherited");
}

/**
 * The slugs a teammate gains when its pinned list is handed back to inheriting.
 *
 * Returning to inherit is not scoped to the skill being edited: it hands the
 * teammate everything the company has enabled. These are the extras, so the
 * panel can name them before the save rather than after.
 */
export function slugsGainedByInheriting(
  requested: string[] | null | undefined,
  companyAvailable: readonly string[],
): string[] {
  if (requested === null || requested === undefined) return [];
  const held = new Set(requested);
  return companyAvailable.filter((slug) => !held.has(slug));
}

/**
 * The sentence the skill panel shows before handing pinned teammates back to
 * inheriting.
 *
 * The mirror of [`SCOPE_PINS_INHERITED_WARNING`], and one string with one test
 * on it for the same reason.
 */
export const SCOPE_CLEARS_TO_INHERITED_WARNING =
  "Choosing all agents hands each pinned teammate back to inheriting, so it gets every skill " +
  "the company has enabled — not only this one. The teammates below gain more than they had.";

/** What a scope can and cannot do, stated on both surfaces that edit one. */
export const SCOPE_ONLY_TAKES_AWAY =
  "A scope only ever takes away — it can never give a teammate a skill the company has disabled.";
