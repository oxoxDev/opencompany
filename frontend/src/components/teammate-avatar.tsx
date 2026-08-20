import { Building2 } from "lucide-react";

import { TEAM_TONES, avatarFor, avatarSrc, initials } from "@/lib/team";
import { cn } from "@/lib/utils";

interface Props {
  name: string;
  tone?: string;
  /** The company's own voice wears the brand mark rather than initials. */
  company?: boolean;
  /**
   * Draw the tone tile without initials.
   *
   * For decorative stacks small enough that no glyph can be read at a size
   * the tile can hold: a 16px facepile fits two letters only below 10px,
   * and below 10px is not a size, it is a bug. The tile's colour still
   * distinguishes one voice from the next, which is the whole of what a
   * facepile claims to say.
   */
  markOnly?: boolean;
  /**
   * The mascot key from `avatarFor`. Optional: a caller holding a `Member`
   * passes the id-seeded one so the face matches everywhere that teammate
   * appears, and a caller with only a name falls back to seeding on that.
   */
  avatar?: string;
  className?: string;
  /**
   * Forwarded to the tile so a spec can name one avatar among several on a page.
   *
   * Declared rather than picked up from a rest spread: a hyphenated prop passes
   * TypeScript's excess-property check on any component, so an undeclared
   * `data-testid` here would type-check happily and then be dropped at render —
   * a selector that silently matches nothing.
   */
  "data-testid"?: string;
}

/**
 * A square-ish chat avatar: initials on a tone-tinted tile.
 *
 * Rounded rather than circular, which is what distinguishes a workspace
 * avatar from a contact-list one — DM rows, message gutters, and the member
 * pane all draw the same tile at different sizes.
 */
export function TeammateAvatar({
  name,
  tone,
  company,
  markOnly,
  avatar,
  className,
  "data-testid": testId,
}: Props) {
  if (company) {
    return (
      <span
        className={cn(
          "flex shrink-0 items-center justify-center rounded-md bg-primary text-primary-foreground",
          className,
        )}
        aria-hidden
        data-testid={testId}
      >
        <Building2 className="size-1/2" />
      </span>
    );
  }

  // `markOnly` is the caller saying "this tile is too small to read". The
  // mascot is subject to the same limit as the initials it replaces — at 16px
  // it is a smudge — so the tone tile stays the answer there rather than a
  // detailed drawing nobody can resolve.
  if (markOnly) {
    return (
      <span
        className={cn(
          "flex shrink-0 items-center justify-center rounded-md text-xs font-semibold",
          toneClass(tone),
          className,
        )}
        aria-hidden
        data-testid={testId}
      />
    );
  }

  // The tone tile stays underneath the image on purpose: it is what shows if
  // the avatar 404s or has not loaded yet, so the gutter never collapses to a
  // blank square mid-scroll.
  return (
    <span
      className={cn(
        "relative flex shrink-0 items-center justify-center overflow-hidden rounded-md text-xs font-semibold",
        toneClass(tone),
        className,
      )}
      aria-hidden
      data-testid={testId}
    >
      <span className="absolute inset-0 flex items-center justify-center">{initials(name)}</span>
      <img
        src={avatarSrc(avatar ?? avatarFor(name))}
        alt=""
        loading="lazy"
        decoding="async"
        className="relative size-full object-cover"
      />
    </span>
  );
}

/** Fall back to a hashed tone so an unnamed voice still gets a stable color. */
function toneClass(tone?: string): string {
  if (tone && TEAM_TONES[tone]) return TEAM_TONES[tone];
  const keys = Object.keys(TEAM_TONES);
  let hash = 0;
  const seed = tone ?? "";
  for (let i = 0; i < seed.length; i++) hash = (hash * 31 + seed.charCodeAt(i)) | 0;
  return TEAM_TONES[keys[Math.abs(hash) % keys.length]];
}
