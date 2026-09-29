// The company's desks: the standing lines you can address. Each one becomes a
// channel in the chat workspace. They all post to the same company endpoint —
// a desk scopes a transcript and fixes the company side's identity, it is not
// a separate backend.

/** What a desk entry is: the company-wide `#general` channel, or an ordinary desk. */
export type DeskKind = "general" | "desk";

/** Is this desk entry the `#general` channel? */
export function isGeneralDesk(desk: { kind?: DeskKind }): boolean {
  return desk.kind === "general";
}

export interface Desk {
  id: string;
  /** The channel name, rendered after a `#`. Lowercase, no spaces. */
  channel: string;
  /** How the desk signs its messages — a person-ish name, not a slug. */
  name: string;
  /** One line on what the desk is for; the channel's purpose. */
  blurb: string;
  /** Avatar tone key. `#general` uses the brand mark instead. */
  tone?: string;
  /**
   * The desk's own members, as roster teammate ids, in the host's order —
   * `members[0]` is the lead. Optional on purpose: the static desks below have
   * no membership at all, and "this desk's membership is unknown" has to stay
   * distinguishable from "this desk has nobody on it". A consumer that finds
   * it absent should fall back to the company-wide roster rather than render
   * an empty channel (issue #369).
   */
  members?: string[];
  /**
   * The subset of {@link members} added through the operator overlay rather
   * than declared in the manifest. Carried through so a later surface can tell
   * the removable members from the blueprint ones without refetching.
   */
  overlayMembers?: string[];
  /** Whether the whole desk was operator-created rather than declared in the manifest. */
  overlayCreated?: boolean;
  /** `"general"` for the `#general` channel; absent on an older host, which means `"desk"`. */
  kind?: DeskKind;
  /**
   * Whether the host accepts membership, order and delete writes for this desk.
   * `false` for `#general`, whose membership is the roster; absent means `true`.
   */
  mutable?: boolean;
  /**
   * How the desk routes its unmentioned messages (issue #1835). `"auto"` is a
   * leadless channel — `members[0]` carries no rank and the host picks a
   * best-fit member per message. Absent means `"lead"`, today's model.
   */
  responder?: "lead" | "auto";
}

/** A few focused desks, for a host that exposes none of its own. */
export function defaultDesks(): Desk[] {
  return [
    {
      id: "strategy",
      channel: "strategy",
      name: "Strategy desk",
      blurb: "Plans, priorities, and direction",
      tone: "sky",
    },
    {
      id: "creative",
      channel: "creative",
      name: "Creative studio",
      blurb: "Copy, design, and campaigns",
      tone: "violet",
    },
    {
      id: "frontdesk",
      channel: "front-desk",
      name: "Front desk",
      blurb: "Scheduling, inbox, and errands",
      tone: "amber",
    },
  ];
}
