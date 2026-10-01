import { randomUUID } from "node:crypto";

import {
  expect,
  test,
  type APIRequestContext,
  type Page,
} from "@playwright/test";

import { hostSkills, installedCard, openSkills, suppressTour } from "./skills";

/**
 * A skill's detail panel, end to end against a live host (#2486).
 *
 * The transpose of `skills-agent-scope.spec.ts`: that one drives the teammate
 * page's picker, this one drives the skill's. Both write the same route, and the
 * thing only a browser can prove is that they agree — the panel's write is
 * computed from the roster read, and a panel that read the per-skill projection
 * alone would send a body the host accepts and that strips the skills nobody
 * touched.
 *
 * Driven against a teammate this spec creates and deletes, for
 * `skills-agent-scope.spec.ts`'s reason: a manifest teammate's reset does not
 * round-trip, so narrowing one would leave every later spec running against a
 * teammate this file silently re-scoped. The probe teammate is created with an
 * explicit narrow scope so unticking it is a plain list edit rather than the
 * one-way materialisation of an inherited one.
 *
 * Default features are enough; every route here ships in the default build.
 */

const AGENT_NAME = `Panel Probe ${randomUUID()}`;
/** The id the host mints for `AGENT_NAME`, captured once it exists. */
let AGENT_ID = "";

async function removeAgent(request: APIRequestContext) {
  if (!AGENT_ID) return;
  await request
    .delete(`/api/v1/company/team/${AGENT_ID}`)
    .catch(() => undefined);
}

/** The company's enabled skills, in the order the host reports them. */
async function enabledSkills(request: APIRequestContext) {
  const enabled = (await hostSkills(request)).filter((skill) => skill.enabled);
  expect(
    enabled.length,
    "the harness company should have enabled skills",
  ).toBeGreaterThan(1);
  return enabled;
}

/** The probe teammate's stored scope, off the host rather than off the screen. */
async function storedScope(
  request: APIRequestContext,
): Promise<string[] | null> {
  const answer = await request.get(`/api/v1/company/team/${AGENT_ID}`);
  expect(
    answer.ok(),
    `reading ${AGENT_ID} failed: ${await answer.text()}`,
  ).toBeTruthy();
  return (await answer.json()).skills.requested;
}

/** Opens the panel from the card, the way the screen flow says to. */
async function openPanel(page: Page, name: string) {
  await openSkills(page);
  const card = installedCard(page, name);
  await expect(card).toBeVisible({ timeout: 30_000 });
  await card.getByTestId("skill-card-open").click();
  await expect(page.getByTestId("skill-detail-name")).toHaveText(name, {
    timeout: 30_000,
  });
}

test.beforeEach(async ({ page, request }) => {
  await suppressTour(page);
  await removeAgent(request);
  const created = await request.post("/api/v1/company/team", {
    data: { name: AGENT_NAME, role: "Probe" },
  });
  expect(
    created.ok(),
    `creating ${AGENT_NAME} failed: ${await created.text()}`,
  ).toBeTruthy();
  AGENT_ID = (await created.json()).id;
});

test.afterEach(async ({ request }) => {
  await removeAgent(request);
});

test("unticking a teammate on the panel narrows that teammate and nothing else", async ({
  page,
  request,
}) => {
  const enabled = await enabledSkills(request);
  const subject = enabled[0];
  const others = enabled.slice(1).map((skill) => skill.id);

  // A narrow scope to start from, so the save is a list edit rather than the
  // materialisation of an inherited one — and so the assertion below has other
  // skills to prove survived.
  const scoped = await request.patch(`/api/v1/company/team/${AGENT_ID}`, {
    data: { skills: [subject.id, ...others] },
  });
  expect(
    scoped.ok(),
    `scoping the probe failed: ${await scoped.text()}`,
  ).toBeTruthy();

  await openPanel(page, subject.name);

  // The roster's other teammates all inherit, so the panel opens on "All agents"
  // and the per-teammate list is behind the second option.
  await page.getByTestId("skill-detail-mode-selected").click();
  const box = page.getByTestId(`skill-agent-toggle-${AGENT_ID}`);
  await expect(box).toBeChecked();
  await box.uncheck();
  await expect(page.getByTestId("skill-detail-save")).toBeEnabled();
  await page.getByTestId("skill-detail-save").click();

  // The panel closes on a clean save, and the host holds the narrowed list —
  // with every other skill the teammate had still on it. A body of
  // `[subject.id]` would have been accepted and would have stripped them.
  await expect(page.getByTestId("skill-page")).toHaveCount(0, {
    timeout: 30_000,
  });
  expect(await storedScope(request)).toEqual(others);

  // And it survives a reload, rather than having narrowed only on screen.
  await openPanel(page, subject.name);
  await page.getByTestId("skill-detail-mode-selected").click();
  await expect(
    page.getByTestId(`skill-agent-toggle-${AGENT_ID}`),
  ).not.toBeChecked();
});

test("ticking a teammate back adds the skill to the list it already holds", async ({
  page,
  request,
}) => {
  const enabled = await enabledSkills(request);
  const subject = enabled[0];
  const others = enabled.slice(1).map((skill) => skill.id);

  // Scoped to everything except the subject: ticking it must produce
  // `[...others, subject]`, never `[subject]`.
  const scoped = await request.patch(`/api/v1/company/team/${AGENT_ID}`, {
    data: { skills: others },
  });
  expect(
    scoped.ok(),
    `scoping the probe failed: ${await scoped.text()}`,
  ).toBeTruthy();

  await openPanel(page, subject.name);
  await page.getByTestId("skill-detail-mode-selected").click();
  const box = page.getByTestId(`skill-agent-toggle-${AGENT_ID}`);
  await expect(box).not.toBeChecked();
  await box.check();
  await page.getByTestId("skill-detail-save").click();
  await expect(page.getByTestId("skill-page")).toHaveCount(0, {
    timeout: 30_000,
  });

  const stored = await storedScope(request);
  expect(
    stored,
    "every skill the teammate already held is still on the list",
  ).toEqual([...others, subject.id]);
});

test("choosing all agents hands a pinned teammate back to inheriting", async ({
  page,
  request,
}) => {
  const enabled = await enabledSkills(request);
  const subject = enabled[0];

  // Pin the probe to a list that leaves something out, which is the state a
  // newly installed skill silently never reaches.
  const scoped = await request.patch(`/api/v1/company/team/${AGENT_ID}`, {
    data: { skills: [subject.id] },
  });
  expect(
    scoped.ok(),
    `pinning the probe failed: ${await scoped.text()}`,
  ).toBeTruthy();
  expect(await storedScope(request), "pinned to start").toEqual([subject.id]);

  await openPanel(page, subject.name);

  // The panel names what the reset widens before it is sent.
  await page.getByTestId("skill-detail-mode-all").click();
  await expect(page.getByTestId("skill-detail-widening-warning")).toBeVisible();

  await expect(page.getByTestId("skill-detail-save")).toBeEnabled();
  await page.getByTestId("skill-detail-save").click();
  await expect(page.getByTestId("skill-page")).toHaveCount(0, {
    timeout: 30_000,
  });

  // `null`, not a list that happens to name everything: only `null` keeps
  // reaching skills installed after this write.
  expect(await storedScope(request)).toBeNull();
});

test("the row menu's Scope… opens the same panel the card does", async ({
  page,
  request,
}) => {
  const subject = (await enabledSkills(request))[0];

  await openSkills(page);
  const card = installedCard(page, subject.name);
  await expect(card).toBeVisible({ timeout: 30_000 });
  await card.getByTestId("skill-row-menu").click();
  await page.getByTestId("skill-menu-scope").click();

  await expect(page.getByTestId("skill-detail-name")).toHaveText(subject.name, {
    timeout: 30_000,
  });
  // The panel names every roster teammate, including the probe — a sparse list
  // would leave the operator guessing what an absent teammate meant.
  await page.getByTestId("skill-detail-mode-selected").click();
  await expect(
    page.getByTestId(`skill-agent-toggle-${AGENT_ID}`),
  ).toBeVisible();
});
