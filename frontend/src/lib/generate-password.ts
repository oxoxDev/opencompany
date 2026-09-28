// A password the console offers the first admin, so "pick a password" is a
// button rather than a chore.

/**
 * The alphabet a generated password draws from.
 *
 * Lowercase and digits with the look-alikes removed (`0/o`, `1/l/i`) — a
 * generated password is one somebody may read off one screen and type into
 * another, and a glyph that can be two characters costs a sign-in attempt. No
 * symbols, for the same reason and because the host's policy is length-only
 * (NIST SP 800-63B; `src/server/users/password.rs`), so they would buy nothing.
 */
const ALPHABET = "abcdefghjkmnpqrstuvwxyz23456789";

/** Characters per dash-separated group. */
const GROUP = 4;

/** Groups per password: 5 × 4 = 20 characters, well past the 12 the host asks. */
const GROUPS = 5;

/**
 * A fresh random password, as `xk7f-2mqp-9vwd-t4hn-r8cb`.
 *
 * ~99 bits from the OS's CSPRNG (`crypto.getRandomValues`), grouped so it can
 * be read aloud or copied by hand. Each byte is reduced modulo the alphabet
 * size, which is 31 — not a divisor of 256, so the distribution is very
 * slightly uneven (the first 8 characters are chosen 9/256 of the time, the
 * rest 8/256). That bias is worth well under a bit across the whole password
 * and is not worth a rejection-sampling loop in a sign-in screen.
 */
export function generatePassword(): string {
  const bytes = new Uint8Array(GROUP * GROUPS);
  crypto.getRandomValues(bytes);
  const chars = Array.from(bytes, (byte) => ALPHABET[byte % ALPHABET.length]);
  const groups: string[] = [];
  for (let i = 0; i < GROUPS; i += 1) {
    groups.push(chars.slice(i * GROUP, (i + 1) * GROUP).join(""));
  }
  return groups.join("-");
}

/**
 * The host's minimum password length, mirrored so the form can say "too
 * short" before a round trip. Must track `MIN_PASSWORD_LEN` in
 * `src/server/users/password.rs`.
 */
export const MIN_PASSWORD_LENGTH = 12;

/**
 * Why a password the person typed will be refused, or `undefined` when it will
 * not. Length only, like the host: no composition rules.
 */
export function passwordProblem(password: string): string | undefined {
  if (password.length < MIN_PASSWORD_LENGTH) {
    return `Use at least ${MIN_PASSWORD_LENGTH} characters.`;
  }
  if (!password.trim()) return "A password can't be only spaces.";
  return undefined;
}
