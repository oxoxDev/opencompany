// The company's desks: the standing lines you can address. Each one becomes a
// channel in the chat workspace. They all post to the same company endpoint —
// a desk scopes a transcript and fixes the company side's identity, it is not
// a separate backend.

export interface Desk {
  id: string;
  /** The channel name, rendered after a `#`. Lowercase, no spaces. */
  channel: string;
  /** How the desk signs its messages — a person-ish name, not a slug. */
  name: string;
  /** One line on what the desk is for; the channel's purpose. */
  blurb: string;
  /** Avatar tone key. The main line uses the brand mark instead. */
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
}

/** The main line plus a few focused desks. */
export function defaultDesks(): Desk[] {
  return [
    {
      id: "main",
      channel: "general",
      name: "Your company",
      blurb: "The main line — ask for anything",
    },
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
