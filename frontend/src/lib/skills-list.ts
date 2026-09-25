// How the installed-skills list labels, filters and orders its rows.
//
// Pure functions, deliberately: the Skills tab shows one company's skills —
// the global baseline plus whatever was installed and authored — so filtering
// and sorting are client-side, and everything here is a value in, a value out.
// That keeps the list's actual rules (what "Registry v1.2" means, which rows a
// filter drops, where an unedited skill sorts) under the fast unit runner
// rather than reachable only through six layers of render.

/** The provenance values the host serves on `Skill.source`. */
export type SkillSourceId = "company" | "registry" | "custom";

/** The provenance filters offered above the list, in the order shown. */
export const SKILL_SOURCE_FILTERS = ["all", "company", "registry", "custom"] as const;
export type SkillSourceFilter = (typeof SKILL_SOURCE_FILTERS)[number];

/** The enabled-state filters offered above the list, in the order shown. */
export const SKILL_ENABLED_FILTERS = ["all", "enabled", "disabled"] as const;
export type SkillEnabledFilter = (typeof SKILL_ENABLED_FILTERS)[number];

/** The orderings offered above the list, in the order shown. */
export const SKILL_SORTS = ["edited", "name"] as const;
export type SkillSort = (typeof SKILL_SORTS)[number];

/** The human label for each ordering. */
export const SKILL_SORT_LABELS: Record<SkillSort, string> = {
  edited: "Last edited",
  name: "Name",
};

/**
 * Why a built-in skill's Uninstall is greyed rather than hidden.
 *
 * The host's own sentence, verbatim: `server::ops::language::BUILTIN_UNINSTALL`
 * is what the route answers when someone tries it anyway, and a host test reads
 * this file to keep the two identical. A console that invented its own wording
 * would explain the refusal one way in the menu and another way in the toast.
 */
/**
 * Why a modified skill's Update is greyed.
 *
 * The host's own sentence, verbatim: `server::ops::language::SKILL_MODIFIED_NO_UPDATE`
 * is what the route answers when someone posts the update anyway, and a host
 * test reads this file to keep the two identical. The same discipline as
 * [`SKILL_BUILTIN_UNINSTALL_REASON`], and for the same reason — two wordings for
 * one refusal means the menu explains it one way and the toast another, and only
 * one of them gets updated when the rule changes.
 */
export const SKILL_MODIFIED_UPDATE_REASON =
  "This skill's text was changed after it was installed. Updating would replace those changes, so it's left to you — uninstall it and install it again to take the registry's version.";

export const SKILL_BUILTIN_UNINSTALL_REASON =
  "This is a built-in skill and can't be uninstalled — you can disable it instead.";

/**
 * Reads a field that is typed as a string but arrives as JSON.
 *
 * Rows reach this module straight from a host response, including the ones an
 * upload or a saved draft folds in optimistically without a re-read. A host
 * that omits a field — an older one, a partial projection — must cost that row
 * its label, not the whole tab: `undefined.trim()` throws inside render and
 * takes every other skill down with it.
 */
function text(value: string | null | undefined): string {
  return typeof value === "string" ? value : "";
}

/** The shape of a row this module orders. A subset of `@/api/skills`'s `Skill`. */
export interface SkillListRow {
  name: string;
  description: string;
  category: string;
  source: string;
  enabled: boolean;
  updatedAtMillis?: number | null;
}

/** Where a row's install stands, as the host reports it on every skill read. */
export interface SkillDriftRow {
  /** The revisions either side of a library change, when there has been one. */
  updateAvailable?: { from?: string | null; to?: string | null } | null;
  /** Whether the stored copy was edited after it was installed. */
  modified?: boolean;
}

/** The badge text for a row whose install has drifted, or `null` for one that
 * has not.
 *
 * **Modified wins over update-available.** Both can be true at once — a locally
 * edited copy of a skill whose library entry also moved — and one badge has to
 * choose. It says the thing that constrains the operator: an update is refused
 * while the copy is modified, so leading with "Update available" would advertise
 * an action the host declines and hide the reason.
 */
export function skillDriftLabel(skill: SkillDriftRow): string | null {
  if (skill.modified) return "Modified";
  if (skill.updateAvailable) return "Update available";
  return null;
}

/** Whether this row's Update action should be offered as enabled.
 *
 * The console mirror of the host's `SkillDrift::update_allowed`: something newer
 * exists, and applying it would not discard an edit. A host test reads this file
 * to keep the two identical — a menu that offered an update the route then
 * refused would teach the operator that the button is unreliable.
 */
export function canUpdateSkill(skill: SkillDriftRow): boolean {
  return !!skill.updateAvailable && !skill.modified;
}

/** Why a row's Update is greyed rather than hidden, or `null` when it is
 * enabled.
 *
 * Cause-specific, and in the same order [`skillDriftLabel`] resolves its two
 * inputs, so the badge and the menu never explain one row differently. The
 * modified case is the host's own sentence verbatim
 * ([`SKILL_MODIFIED_UPDATE_REASON`]), because that is the refusal an operator
 * can also reach by posting the update anyway.
 */
export function skillUpdateUnavailableReason(
  skill: SkillDriftRow & Pick<SkillListRow, "source">,
): string | null {
  if (canUpdateSkill(skill)) return null;
  if (skill.modified) return SKILL_MODIFIED_UPDATE_REASON;
  if (skill.source !== "registry") {
    return "Only a skill installed from the registry can be updated.";
  }
  return "This skill already matches the registry.";
}

/** What the filter controls above the list currently select. */
export interface SkillListFilters {
  /** Free text matched against name and description. */
  query: string;
  source: SkillSourceFilter;
  enabled: SkillEnabledFilter;
  /** A category name, or `"all"`. */
  category: string;
}

/** Nothing filtered out, newest edit first — what the list opens on. */
export const DEFAULT_SKILL_FILTERS: SkillListFilters = {
  query: "",
  source: "all",
  enabled: "all",
  category: "all",
};

/**
 * The provenance label on a row: "Company", "Registry v1.2", "Custom".
 *
 * The version rides the registry label because that is the only place it means
 * anything — it is the library revision the install snapshotted. A custom or
 * company skill has no library copy to be a revision *of*, so showing one there
 * would imply a comparison that cannot be made.
 *
 * An unrecognised source is title-cased rather than dropped: the host owns this
 * vocabulary, and a console that silently rendered nothing for a value it did
 * not know would hide the row's provenance entirely.
 */
export function skillSourceLabel(skill: Pick<SkillListRow, "source"> & { version?: string | null }): string {
  const source = text(skill.source).trim();
  if (source === "registry") {
    const version = text(skill.version).trim();
    if (!version) return "Registry";
    return `Registry ${version.toLowerCase().startsWith("v") ? version : `v${version}`}`;
  }
  if (source === "company") return "Company";
  if (source === "custom") return "Custom";
  return source ? source[0].toUpperCase() + source.slice(1) : "Unknown";
}

/**
 * Whether this row's skill can be uninstalled at all.
 *
 * Mirrors the host's uninstall arm, which removes a `Registry` or `Custom`
 * delta and refuses everything else. Kept as a predicate rather than inlined so
 * the menu's disabled state and the reason beside it cannot disagree.
 */
export function canUninstallSkill(source: string): boolean {
  return source === "registry" || source === "custom";
}

/** Only custom skills are editable in the console: nothing else was authored here. */
export function canEditSkill(source: string): boolean {
  return source === "custom";
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/**
 * When the skill was last written, in words.
 *
 * `null`/`undefined` is "never edited here", not a date: a baseline or bundled
 * skill is authored in the repository and a row stored before the host recorded
 * timestamps has no edit to report. Both would otherwise render as 1970.
 *
 * A stamp in the future is clock skew between the host and this browser, not a
 * scheduled edit, so it reads as "just now" rather than counting backwards.
 */
export function skillLastEditedLabel(
  millis: number | null | undefined,
  now: number,
  locale?: string,
): string {
  if (millis === null || millis === undefined) return "Never edited";
  const elapsed = now - millis;
  if (elapsed < MINUTE) return "Edited just now";
  if (elapsed < HOUR) return `Edited ${plural(Math.floor(elapsed / MINUTE), "minute")} ago`;
  if (elapsed < DAY) return `Edited ${plural(Math.floor(elapsed / HOUR), "hour")} ago`;
  if (elapsed < 30 * DAY) return `Edited ${plural(Math.floor(elapsed / DAY), "day")} ago`;
  return `Edited ${new Date(millis).toLocaleDateString(locale, {
    year: "numeric",
    month: "short",
    day: "numeric",
  })}`;
}

function plural(count: number, unit: string): string {
  return `${count} ${unit}${count === 1 ? "" : "s"}`;
}

/**
 * Every category present in the list, for the category filter's options.
 *
 * Read off the rows rather than from a fixed list, because the host's category
 * is a free-form string out of the skill's own frontmatter — a filter built
 * from a hardcoded set would silently offer no way to find a skill filed under
 * anything else.
 */
export function skillCategories(skills: readonly SkillListRow[]): string[] {
  return [...new Set(skills.map((s) => text(s.category)).filter((c) => c.trim() !== ""))].sort(
    (a, b) => a.localeCompare(b),
  );
}

/**
 * The rows the list should render, filtered and ordered.
 *
 * Never mutates its input: the view holds `skills` as state and React compares
 * it by identity.
 *
 * Under `edited`, a row with no stamp sorts **after** every row that has one.
 * It is not the oldest edit — it is the absence of one, and putting the
 * company's untouched baseline at the top of a list titled "last edited" would
 * say the opposite of what happened. Name is the tie-break in both orderings,
 * so two skills written in the same millisecond do not swap places between
 * renders.
 */
export function visibleSkills(
  skills: readonly SkillListRow[],
  filters: SkillListFilters,
  sort: SkillSort,
): SkillListRow[] {
  const q = filters.query.trim().toLowerCase();
  const matching = skills.filter((skill) => {
    if (
      q &&
      !text(skill.name).toLowerCase().includes(q) &&
      !text(skill.description).toLowerCase().includes(q)
    )
      return false;
    if (filters.source !== "all" && skill.source !== filters.source) return false;
    if (filters.enabled === "enabled" && !skill.enabled) return false;
    if (filters.enabled === "disabled" && skill.enabled) return false;
    if (filters.category !== "all" && skill.category !== filters.category) return false;
    return true;
  });

  const byName = (a: SkillListRow, b: SkillListRow) => text(a.name).localeCompare(text(b.name));
  if (sort === "name") return matching.sort(byName);
  return matching.sort((a, b) => {
    const left = a.updatedAtMillis ?? null;
    const right = b.updatedAtMillis ?? null;
    if (left === null && right === null) return byName(a, b);
    if (left === null) return 1;
    if (right === null) return -1;
    return right - left || byName(a, b);
  });
}
