import { expect, test } from "@playwright/test";

/**
 * Regression proof for #1333 — the "check your email" card must carry the
 * recovery action, and the chrome around it must stop advertising the form it
 * replaced.
 *
 * This is the terminal screen of the primary sign-in path and the one an
 * operator stares at when the mail is slow, filtered, or already closed in
 * another tab. Before this fix its only control was "Use a different address",
 * which cleared the form: the modelled recovery from "it didn't arrive" was to
 * retype the address you had just typed, into a screen that would not say it
 * was a *re*send and would not mention the minute the host makes you wait
 * (`RESEND_INTERVAL_MILLIS`, `src/server/users/routes.rs`). Around it, the
 * subtitle still read "We'll email you a link" — future tense, about a form no
 * longer on screen — and "Use a password instead" still offered to throw the
 * link away without saying so.
 *
 * Driven in a browser rather than as a unit test because every claim here is
 * about what is on screen at once: three lines that must agree, and a control
 * that must exist, must refuse to fire, and must then fire. The clock
 * arithmetic itself is covered in `test/unit/login-resend.test.ts`.
 */

// Signed out, which is the only state that renders this view at all. The shared
// `storageState` holds a live session, so it must be dropped rather than
// worked around.
test.use({ storageState: { cookies: [], origins: [] } });

/**
 * An address the harness company does not admit.
 *
 * Deliberate: the host answers a stranger and a member identically here, by
 * design, so a stranger exercises exactly the screen under test while minting
 * no code and invalidating no link the rest of the suite is holding.
 */
const STRANGER = "nobody-1333@example.com";

const SUBTITLE = "We'll email you a link. No password needed.";

/**
 * The harness host binds loopback with no mail transport, and a host that
 * cannot mail draws no link form at all — the password is its one sign-in.
 * The screen under test is the mailed-link one, so the host's answer is
 * stubbed to say it mails; `auth/request` itself still answers the real host,
 * which acknowledges a stranger exactly as it does a member.
 */
const MAILING_HOST = { mode: "email", passwords: true, magicLink: true, claimable: false };

test("the sent card offers a throttled resend, and nothing around it contradicts it", async ({
  page,
}) => {
  // The throttle window is a minute of real time, which is longer than this
  // spec's whole budget. A fake clock is the difference between asserting that
  // the button unlocks and asserting only that it locks — and it is the
  // unlocking half that proves the countdown is a wait rather than a dead end.
  await page.clock.install();
  await page.route("**/auth/config", (route) => route.fulfill({ json: MAILING_HOST }));
  await page.goto("/");

  const emailField = page.getByLabel("Email");
  await expect(emailField).toBeVisible();
  // The pre-send screen is the control for two of the assertions below: both
  // of these are correct *here* and wrong once the link is out.
  await expect(page.getByText(SUBTITLE)).toBeVisible();
  await expect(page.getByRole("button", { name: "Use a password instead" })).toBeVisible();

  await emailField.fill(STRANGER);
  await page.getByRole("button", { name: "Email me a link" }).click();

  await expect(page.getByText("Check your email")).toBeVisible();

  // 1. The recovery action exists, and says why it will not fire yet. The wait
  //    is in the label rather than beside it, so a screen reader announcing the
  //    disabled button announces the reason with it.
  const resend = page.getByTestId("login-resend");
  await expect(resend).toBeVisible();
  await expect(resend).toBeDisabled();
  await expect(resend).toHaveText(/Resend link in \d+s/);

  // 2. The subtitle no longer describes the form that is gone.
  await expect(page.getByText(SUBTITLE)).toHaveCount(0);

  // 3. The credential toggle is not offered as a peer of "go look in your
  //    mailbox" — pressing it discarded the link with no acknowledgement.
  await expect(page.getByRole("button", { name: "Use a password instead" })).toHaveCount(0);

  // 4. The counter is live, and it counts the host's window rather than some
  //    shorter one of the console's own invention.
  await page.clock.fastForward("00:30");
  await expect(resend).toHaveText("Resend link in 30s");
  await expect(resend).toBeDisabled();

  await page.clock.fastForward("00:30");
  await expect(resend).toHaveText("Resend link");
  await expect(resend).toBeEnabled();

  // 5. Pressing it says so. Silence here is the original complaint restated:
  //    a second press with no acknowledgement leaves you unable to tell a
  //    resend from a click that did nothing.
  await resend.click();
  await expect(page.getByTestId("login-resent")).toBeVisible();
  await expect(resend).toBeDisabled();
  await expect(resend).toHaveText(/Resend link in \d+s/);

  // 6. The way back to the form is still there, and still works — which is
  //    what makes hiding the toggle a demotion rather than a dead end.
  await page.getByRole("button", { name: "Use a different address" }).click();
  await expect(emailField).toBeVisible();
  await expect(page.getByText(SUBTITLE)).toBeVisible();
  await expect(page.getByRole("button", { name: "Use a password instead" })).toBeVisible();
});
