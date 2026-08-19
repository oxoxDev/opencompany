/**
 * The workflow id's character rule, and the one derivation from a name.
 *
 * Its own module rather than an addition to `workflow-draft` so it can be tested
 * without a render, and so the create dialog's id rules sit in one place instead
 * of a predicate in the component and a slugger beside it.
 */

/**
 * The **single** source of what an id may contain.
 *
 * Both the check and the derivation are built from this string. Issue #1053's
 * complaint was that the form rejected work it could have derived; the trap in
 * fixing that is writing the character rule down twice — once to test an id and
 * once to make one — after which a slugger that emits a character the checker
 * refuses produces an error message about the operator's *own* generated id.
 */
const SAFE_ID_CHARS = "A-Za-z0-9_-";

/** A safe on-disk id: only letters, digits, `_`, and `-` — a subset of what the
 * host's `safe_wid` accepts (any single path component), chosen to keep ids
 * simple and unambiguous without a round-trip to the server first. */
export function isSafeId(id: string): boolean {
  return new RegExp(`^[${SAFE_ID_CHARS}]+$`).test(id);
}

/**
 * Derives an id from a workflow name (issue #1053).
 *
 * `"Weekly digest"` → `"weekly-digest"`. Guaranteed to satisfy
 * {@link isSafeId} or to be empty — never something the form would then reject,
 * which is the whole point of deriving it from the same character class.
 *
 * Lower-cased because an id is a path component and two ids differing only in
 * case are a trap on a case-insensitive filesystem. Runs of unusable characters
 * collapse to a single `-`, and leading/trailing separators are trimmed, so
 * `"  Weekly   digest!!  "` gives `"weekly-digest"` rather than `"--weekly---digest--"`.
 *
 * Returns `""` when the name has nothing usable in it (`"???"`, `"日本語"`). The
 * caller must treat that as "no id derived" and leave the field alone: an empty
 * id fails {@link isSafeId}, and silently writing one would replace the
 * operator's clear error with a mystery.
 */
export function slugifyWorkflowId(name: string): string {
  return name
    .trim()
    .toLowerCase()
    .replace(new RegExp(`[^${SAFE_ID_CHARS}]+`, "g"), "-")
    .replace(/^-+|-+$/g, "");
}
