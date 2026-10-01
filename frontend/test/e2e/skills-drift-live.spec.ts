import { expect, test, type Page } from "@playwright/test";

import { DriftHost } from "./drift-host";
import {
  installedCard,
  markdownUpload,
  skillDoc,
  suppressTour,
} from "./skills";

/**
 * Drift the host actually computed, end to end.
 *
 * The rest of the Skills suite drives the managed host, whose library is the
 * repository's own `companies/` tree — loaded once per process, so no console
 * action can make it move under an install and `updateAvailable` is not a state
 * a browser can reach there. `skills-list-view.spec.ts` covers the console half
 * over a rewritten response and says so; the Rust provenance tests cover the
 * arithmetic. Between them sat the seam neither could close: that a host whose
 * library genuinely moved sends what the console is built to read.
 *
 * This closes it with a host of its own (`drift-host.ts`), over a library that
 * is one file this spec writes:
 *
 *   1. boot, and install the library's v1 — the install pins its digest;
 *   2. stop, rewrite the library to v2, boot again over the same data root;
 *   3. drive the console: the badge, the Updates filter, and applying it.
 *
 * Nothing is intercepted. The only thing this spec tells the host is what the
 * library says, which is the one input an operator's registry really does have.
 *
 * Default features are enough — every route here ships in the default build.
 */

const SLUG = "cold-outreach";
const NAME = "Cold Outreach";
/** The library entry this spec starts from, copied out of a shipped bundle. */
const SEED = "companies/enterprise_sales/skills/cold-outreach/SKILL.md";

/**
 * One host per test, not one per file: each case rewrites the library and
 * re-pins the install, so a shared host would hand the second test the first
 * one's library and a slug already installed from it.
 */
let host: DriftHost;

test.beforeEach(async () => {
  host = await DriftHost.create("skills", { slug: SLUG, from: SEED });
  await host.start();
});

test.afterEach(async () => {
  await host?.dispose();
});

/** The host's own answer for `SLUG`, read through the signed-in context. */
async function servedSkill(page: Page) {
  const answer = await page.request.get("/api/v1/company/skills");
  expect(answer.ok(), `GET …/skills failed: ${answer.status()}`).toBeTruthy();
  const rows = (await answer.json()) as {
    id: string;
    version?: string | null;
    updateAvailable?: { from?: string | null; to?: string | null } | null;
    modified?: boolean;
  }[];
  const row = rows.find((skill) => skill.id === SLUG);
  expect(row, `${SLUG} should be installed`).toBeTruthy();
  return row!;
}

test("a library that moves under an install is offered as an update, and applying it takes it", async ({
  browser,
}) => {
  const context = await host.signIn(browser);
  try {
    const page = await context.newPage();
    await suppressTour(page);

    // 1. Install what the library says today. The registry is this spec's one
    //    file, so the version pinned here is a version it chose.
    const installed = await page.request.post(
      `/api/v1/company/skills/${SLUG}/install`,
      { data: {} },
    );
    expect(
      installed.ok(),
      `installing ${SLUG} failed: ${await installed.text()}`,
    ).toBeTruthy();

    const pinned = await servedSkill(page);
    expect(pinned.version).toBe("1.0.0");
    expect(
      pinned.updateAvailable ?? null,
      "a fresh install matches the library it came from",
    ).toBeNull();

    // The console agrees there is nothing to offer.
    await page.goto("/#/connections/skills?view=cards");
    const card = installedCard(page, NAME);
    await expect(card).toBeVisible({ timeout: 30_000 });
    await expect(card.getByTestId("skill-update-available")).toHaveCount(0);

    // 2. The library moves. Only a restart reads it, which is the whole reason
    //    this state needs a host of its own.
    await host.stop();
    host.publish(SLUG, (doc) =>
      doc
        .replace(/^version: .*$/m, "version: 1.1.0")
        .concat(
          "\n## Follow-up\n\nIf nobody replies in four days, nudge once.\n",
        ),
    );
    await host.start();

    // 3. The host computed it, from the digest it pinned against the one the
    //    library now renders.
    const drifted = await servedSkill(page);
    expect(drifted.version, "the company still reads what it installed").toBe(
      "1.0.0",
    );
    expect(drifted.updateAvailable).toEqual({ from: "1.0.0", to: "1.1.0" });
    expect(drifted.modified, "nobody edited the stored copy").toBeFalsy();

    // The console says so, and the Updates filter collects it. Reloaded, not
    // navigated: the address has not changed, and the list it is already
    // showing was read from the host that has since been replaced.
    await page.reload();
    await expect(card.getByTestId("skill-update-available")).toBeVisible({
      timeout: 30_000,
    });
    await page.getByTestId("skills-filter-drift").click();
    await page.getByRole("option", { name: "Has update", exact: true }).click();
    await expect(page.getByTestId("installed-card")).toHaveCount(1);
    await expect(page.getByTestId("installed-card")).toContainText(NAME);

    // 4. The review names both revisions off the real library, not off a
    //    constant — the registry column is read from the running host.
    await card.getByTestId("skill-row-menu").click();
    const update = page.getByTestId("skill-menu-update");
    await expect(update).toBeEnabled();
    await update.click();

    const dialog = page.getByTestId("skill-update-dialog");
    await expect(dialog).toBeVisible();
    await expect(dialog.getByTestId("skill-update-versions")).toContainText(
      "1.0.0",
    );
    await expect(dialog.getByTestId("skill-update-versions")).toContainText(
      "1.1.0",
    );
    await expect(dialog.getByTestId("skill-update-live")).toContainText(
      "1.1.0",
    );

    // Keep is the way out that changes nothing, and it has to actually leave
    // the install where it was.
    await dialog.getByTestId("skill-update-keep").click();
    await expect(dialog).toHaveCount(0);
    expect((await servedSkill(page)).version, "Keep must not write").toBe(
      "1.0.0",
    );

    // 5. Applying it takes the library's newer document.
    await card.getByTestId("skill-row-menu").click();
    await page.getByTestId("skill-menu-update").click();
    await page.getByTestId("skill-update-confirm").click();

    await expect
      .poll(async () => (await servedSkill(page)).version, {
        message:
          "the applied update should move the company onto the new revision",
        timeout: 30_000,
      })
      .toBe("1.1.0");
    const applied = await servedSkill(page);
    expect(
      applied.updateAvailable ?? null,
      "nothing is left to offer once it is applied",
    ).toBeNull();
    expect(
      applied.modified,
      "applying the library's own document is not an edit",
    ).toBeFalsy();
  } finally {
    await context.close();
  }
});

test("a row the library moved under is offered even when its own copy was edited", async ({
  browser,
}) => {
  // The two halves of drift are separate answers, and this is the row where
  // they disagree: the library has moved AND somebody edited the stored copy,
  // so the update is refused while the row still belongs under "Has update".
  // Filtering on whether the update can be applied would hide exactly the row
  // that needs a human decision, which is the choice `skills-list.ts` documents
  // and this pins against a host that really is in that state.
  const context = await host.signIn(browser);
  try {
    const page = await context.newPage();
    await suppressTour(page);

    const installed = await page.request.post(
      `/api/v1/company/skills/${SLUG}/install`,
      { data: {} },
    );
    expect(
      installed.ok(),
      `installing ${SLUG} failed: ${await installed.text()}`,
    ).toBeTruthy();

    // Edit the company's own copy: an upload over an installed slug keeps the
    // pin and replaces the document, so the stored copy stops matching it.
    await page.goto("/#/connections/skills?view=cards");
    await expect(installedCard(page, NAME)).toBeVisible({ timeout: 30_000 });
    await page.getByTestId("skills-add-menu").click();
    await page.getByTestId("skills-add-upload").click();
    const dialog = page.getByRole("dialog");
    await dialog.getByTestId("skill-upload-input").setInputFiles(
      markdownUpload(
        "cold-outreach.md",
        skillDoc({
          name: NAME,
          description: "Our own take on the first touch.",
          category: "Marketing",
          body: "Lead with the reason this prospect, not that one.",
        }),
      ),
    );
    await dialog.getByRole("button", { name: "Upload", exact: true }).click();
    await expect(dialog.getByTestId("skill-upload-row").first()).toBeVisible({
      timeout: 30_000,
    });
    await page.keyboard.press("Escape");
    await expect
      .poll(async () => (await servedSkill(page)).modified, { timeout: 30_000 })
      .toBe(true);

    // Now move the library underneath that edit.
    await host.stop();
    host.publish(SLUG, (doc) =>
      doc.replace(/^version: .*$/m, "version: 2.0.0"),
    );
    await host.start();

    const both = await servedSkill(page);
    expect(both.updateAvailable).toEqual({ from: "1.0.0", to: "2.0.0" });
    expect(both.modified, "the stored copy was edited").toBe(true);

    await page.reload();
    const card = installedCard(page, NAME);
    // Modified is the badge that outranks the update, because it is the one
    // that says why the update cannot be taken.
    await expect(card.getByTestId("skill-modified")).toBeVisible({
      timeout: 30_000,
    });

    // Collected by the filter all the same.
    await page.getByTestId("skills-filter-drift").click();
    await page.getByRole("option", { name: "Has update", exact: true }).click();
    await expect(page.getByTestId("installed-card")).toHaveCount(1);
    await expect(page.getByTestId("installed-card")).toContainText(NAME);

    // And the menu refuses it, naming the edit rather than the version.
    await card.getByTestId("skill-row-menu").click();
    await expect(page.getByTestId("skill-menu-update")).toHaveAttribute(
      "aria-disabled",
      "true",
    );
    await expect(page.getByTestId("skill-menu-update-reason")).toContainText(
      "changed after it was installed",
    );
    await page.keyboard.press("Escape");

    // The host refuses it too, so the greyed control is not the only guard.
    const refused = await page.request.post(
      `/api/v1/company/skills/${SLUG}/update`,
      { data: { force: false } },
    );
    expect(refused.status()).toBe(409);
    expect(await refused.text()).toContain("changed after it was installed");
  } finally {
    await context.close();
  }
});
