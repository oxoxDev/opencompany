// The company's team: the agents that do the work. When the host exposes its
// roster (`GET .../team`) the console shows that; otherwise it starts from a
// generic, company-agnostic roster the operator can edit. Either way, agents
// are user-definable here.

import type { AgentDeskDto, TeamMemberDto } from "@/api/types";

/** A desk a teammate sits on, as the roster read reports it. */
export type TeamMemberDesk = AgentDeskDto;

export interface TeamMember {
  id: string;
  name: string;
  role: string;
  description: string;
  /** Avatar tone key; derived from the id so colors stay stable. */
  tone: string;
  /** Mascot avatar key; derived from the same seed, for the same reason. */
  avatar: string;
  /**
   * Whether this teammate has an inbox on the host. Read from `GET …/team` and
   * written by `PUT …/team/{id}/inbox` — never guessed client-side, so the Inbox
   * page and this toggle agree on the same `InboxStore` state (issue #173).
   */
  inboxEnabled: boolean;
  /**
   * The teammate's daily spend cap in USD, as the host will enforce it — an
   * admin's console override when one is set, otherwise the company's own
   * default. Undefined means uncapped: the card shows no budget line at all
   * rather than "$0".
   */
  budgetUsdDaily?: number;
  /** What this teammate has spent since 00:00 UTC; only meaningful with a cap. */
  spentTodayUsd?: number;
  /**
   * The admin who last set this teammate's cap from the console, when one did.
   * Undefined means the cap (if any) is just the company default.
   *
   * Its presence is what makes "set" and "reset to default" different actions
   * in the UI, and it is set even for an override that removed a cap — so an
   * operator can see that a person uncapped this teammate rather than that it
   * was never capped.
   */
  budgetSetBy?: string;
  /** When that cap was set (epoch millis). Paired with `budgetSetBy`. */
  budgetSetAtMillis?: number;
  /**
   * The cognition tier this company declared for the teammate, verbatim
   * (issue #643). Undefined means it declared none — render that as "not
   * declared", never as a default tier.
   */
  tier?: string;
  /**
   * Whether the host resolved this teammate as the company's orchestrator.
   *
   * A separate question from `tier`, answered by the host's roster rule: an
   * untagged company still has an orchestrator, and a second teammate tagged
   * with the orchestrator tier is not one. Undefined means the host does not
   * answer it.
   */
  isOrchestrator?: boolean;
  /**
   * The tool grants this teammate **actually holds** — its own `[[agent]].tools`
   * line narrowed by the company's `[tools].allow`, resolved by the host
   * (issue #601).
   *
   * The `effective` list and only that: the agent detail view shows the same
   * one, from the same server-side function the harness builds the agent with,
   * so the two surfaces cannot disagree about a teammate. Empty means either
   * "holds nothing" or "this host does not report grants" — both draw no tools,
   * and neither is a licence to invent some.
   */
  effectiveTools: string[];
  /**
   * The desks this teammate sits on, host order (manifest desks first, then
   * operator-created ones). Empty means it sits on none.
   *
   * This is the company's real grouping axis, and the overview graph's
   * department pillars are drawn from it.
   */
  desks: TeamMemberDesk[];
}

const TONE_KEYS = ["sky", "violet", "amber", "emerald", "rose", "cyan", "indigo", "teal"];

export function toneFor(seed: string): string {
  let hash = 0;
  for (let i = 0; i < seed.length; i++) hash = (hash * 31 + seed.charCodeAt(i)) | 0;
  return TONE_KEYS[Math.abs(hash) % TONE_KEYS.length];
}

/**
 * The mascot avatars shipped in `public/avatars/`, one file per colourway.
 *
 * Eleven rather than the eight `TONE_KEYS` holds on purpose: the tones are a
 * hue circle that deliberately avoids amber, green and red, while the mascots
 * have no such constraint. Keeping the lists separate stops one being trimmed
 * to match the other.
 */
const AVATAR_KEYS = [
  "amber",
  "blue",
  "clay",
  "cloud",
  "ember",
  "graphite",
  "green",
  "indigo",
  "rose",
  "teal",
  "violet",
];

/**
 * Picks a teammate's mascot from the same seed [`toneFor`] uses.
 *
 * A hash rather than a random draw, for the reason that matters to an
 * operator: a teammate keeps the same face across reloads, browsers and
 * machines, with nothing persisted anywhere. Drawing randomly at creation
 * would need a stored field, and drawing randomly at render would give the
 * same teammate a new face every time the page reloaded.
 *
 * Seeded with the id where there is one (`toneFor` is called the same way), so
 * renaming a teammate does not change its face.
 */
export function avatarFor(seed: string): string {
  let hash = 0;
  for (let i = 0; i < seed.length; i++) hash = (hash * 31 + seed.charCodeAt(i)) | 0;
  return AVATAR_KEYS[Math.abs(hash) % AVATAR_KEYS.length];
}

/** Where [`avatarFor`]'s key lives on disk. */
export function avatarSrc(key: string): string {
  return `/avatars/blob-${key}.webp`;
}

export function initials(name: string): string {
  return (
    name
      .trim()
      .split(/\s+/)
      .slice(0, 2)
      .map((p) => p.charAt(0).toUpperCase())
      .join("") || "?"
  );
}

/** Map a host roster entry into the console's team model. */
export function fromDto(dto: TeamMemberDto): TeamMember {
  const name = dto.name?.trim() || dto.role;
  return {
    id: dto.id,
    name,
    role: dto.role,
    description: dto.description ?? "",
    tone: toneFor(dto.id || name),
    avatar: avatarFor(dto.id || name),
    inboxEnabled: dto.inboxEnabled ?? false,
    // Carried through as-is: `undefined` means uncapped and must stay
    // `undefined`, never coalesced to `0`.
    budgetUsdDaily: dto.budgetUsdDaily,
    spentTodayUsd: dto.spentTodayUsd,
    // Same rule for the attribution — `undefined` means "no override stored",
    // which the card renders differently from "an admin set this".
    budgetSetBy: dto.budgetSetBy,
    budgetSetAtMillis: dto.budgetSetAtMillis,
    // Same rule again (issue #643): `undefined` means the company declared no
    // tier, or the host predates the field. Both are "cannot say" — coalescing
    // either into a tier string is the bug this closed.
    tier: dto.tier,
    isOrchestrator: dto.isOrchestrator,
    // A host predating issue #601 sends neither, and an empty list is the
    // honest reading of that: it draws no tools and no desk rather than a
    // guess at either.
    effectiveTools: dto.tools?.effective ?? [],
    desks: dto.desks ?? [],
  };
}

/**
 * A stable id for a teammate the console invented (issue #364).
 *
 * Derived from the role, not minted from a counter. The counter was the reason
 * nothing about a console-only teammate survived a reload: `member-3` named a
 * different person on the next mount — or nobody — so the DM's URL, its unread
 * count, and the transcript the host journaled under that id all pointed at
 * someone else. A role slug is the same on every mount of every tab.
 *
 * The hash suffix is not decoration. Slugifying keeps only `[a-z0-9]`, so a
 * role written in a non-Latin script slugifies to nothing and two roles that
 * slugify alike collide — and a collision here means two teammates sharing one
 * DM, with their messages merged. The hash of the full role keeps the id
 * readable *and* distinct.
 */
export function localMemberId(role: string): string {
  const trimmed = role.trim();
  const slug = trimmed.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
  return `member-${slug ? `${slug}-` : ""}${roleHash(trimmed)}`;
}

function roleHash(role: string): string {
  let hash = 0;
  for (let i = 0; i < role.length; i++) hash = (hash * 31 + role.charCodeAt(i)) | 0;
  return (hash >>> 0).toString(36);
}

/**
 * A generic starter team that fits any company; the operator edits from here.
 *
 * It spans the functional areas a small company actually splits into — product,
 * engineering, design, growth, operations — rather than a handful of generic
 * roles, so a company that has not defined its own roster still reads as an org
 * rather than a list.
 */
// `starterTeam()` used to live here: twelve invented agents ("Ops Lead", "Front
// Desk", "Product Lead", …) rendered whenever the host's roster came back empty
// or unreadable. It is deleted rather than deprecated.
//
// It was there to keep the console from looking bare, and the cost was that
// every surface lied. The Team page offered budgets and inboxes for teammates
// the host had never heard of; Chat offered DMs whose first message went
// nowhere; the Overview graph drew a full org chart for a company with nobody in
// it. First-run setup replaces the reason it existed — an unstaffed company now
// gets offered a real team it can create — so the honest empty state is what
// remains. See `docs/spec/runtime/company-setup.md`.


/**
 * Create a member from operator-entered fields, for a host with no team write
 * plane — so the id is console-derived and, since issue #364, reload-stable.
 *
 * Keyed on the **name** rather than the role: an operator adding a teammate by
 * hand is naming a person, and two of them may well share a role ("Engineer").
 * The starter roster keys on role because that is what distinguishes its
 * fabricated rows.
 */
export function newMember(fields: { name: string; role: string; description: string }): TeamMember {
  const memberId = localMemberId(fields.name);
  return {
    id: memberId,
    name: fields.name.trim(),
    role: fields.role.trim(),
    description: fields.description.trim(),
    tone: toneFor(memberId),
    avatar: avatarFor(memberId),
    inboxEnabled: false,
    // Nothing on a host has granted this teammate anything or seated it
    // anywhere yet, so both are stated empty rather than guessed.
    effectiveTools: [],
    desks: [],
  };
}

/**
 * The tile colours behind a desk's or teammate's initials.
 *
 * **The keys are legacy slot names, not colour claims.** They are persisted
 * against desks and members and arrive from the host, so they cannot be
 * renamed; what they resolve to is the console's identity palette
 * (`--tone-*`), which deliberately avoids amber, green and red.
 *
 * That avoidance is the point. These tones used to be drawn from the same
 * Tailwind palette as run status, so a desk keyed `emerald` was tinted the
 * exact green that means "done", and one keyed `rose` the red that means
 * "failed" — a colour saying two different things on the same screen. Five
 * tones rather than eight, because five hues clear of the status vocabulary
 * is what the hue circle has room for; a hash over five still gives a stable,
 * well-distributed colour per name.
 *
 * See `--tone-1` in index.css and docs/design-system/color.md.
 */
export const TEAM_TONES: Record<string, string> = {
  violet: "bg-tone-1/15 text-tone-1-text",
  indigo: "bg-tone-1/15 text-tone-1-text",
  sky: "bg-tone-2/15 text-tone-2-text",
  cyan: "bg-tone-2/15 text-tone-2-text",
  teal: "bg-tone-3/15 text-tone-3-text",
  emerald: "bg-tone-3/15 text-tone-3-text",
  rose: "bg-tone-4/15 text-tone-4-text",
  amber: "bg-tone-5/15 text-tone-5-text",
};
