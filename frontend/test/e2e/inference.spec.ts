import { expect, test } from "@playwright/test";

/**
 * Issue #265 — Connections → Inference must never report a successful save for
 * a save that threw the operator's key away.
 *
 * The invariant is unchanged; what upholds it is not. Managed used to be a
 * revert (`DELETE …/inference`) that could carry no credential, so a key typed
 * under a BYOK provider and left in form state by a switch back to managed was
 * dropped while the toast still said "Inference updated". That was first fixed
 * by refusing the save.
 *
 * Issue #585 made the refusal unnecessary: the company's own key on the managed
 * provider is the ordinary case, not a BYOK edge — it keeps the platform
 * endpoint and swaps only the credential — so a managed save carrying a key is
 * now a real `PUT` override that *stores* it. Nothing is discarded, so there is
 * nothing to refuse. These tests assert the invariant against the new mechanism:
 * a typed key survives the switch and lands server-side.
 *
 * This spec drives a real browser against a real host, and the `Console E2E` job
 * runs it (issue #428) — the "not part of CI" note this header used to carry
 * predates that job and was already stale. It is a merge gate, so treat a red
 * run here as a real regression rather than a stale reproduction.
 */

type Page = import("@playwright/test").Page;

/** Pick a provider from the base-ui select. */
async function pickProvider(page: Page, label: string) {
  await page.locator("#inference-provider").click();
  await page.getByRole("option", { name: label, exact: true }).click();
}

/**
 * A fresh browser context has no tour state, so the first-run welcome dialog
 * opens over the console and swallows clicks. Skip it when it shows up.
 */
async function openConnections(page: Page) {
  // Inference has a page of its own since the Connections split — it was a
  // section on the accounts page, which is the wrong neighbourhood for the
  // question it settles.
  await page.goto("/#/settings/inference");
  const skip = page.getByRole("button", { name: "Skip for now" });
  await skip
    .waitFor({ state: "visible", timeout: 10_000 })
    .then(() => skip.click())
    .catch(() => {
      /* already seen in this context — nothing to dismiss */
    });
}

test("the managed brain is offered as something to switch to", async ({ page }) => {
  await openConnections(page);

  // The company has no `[inference]` section, so the host still answers
  // `provider: "managed"`. `INFERENCE_MANAGED_HIDDEN` used to make the console
  // pretend that was "Not configured" and fall the form back to OpenRouter,
  // because choosing it meant minting a TinyHumans key by hand with nowhere in
  // the console to do it. The one-click connect flow (`ConnectTinyHumansButton`)
  // removed that gap, so hiding the route stopped being honest — a company
  // already on it now sees its own real state, and it is a route an operator
  // can actually finish setting up from here.
  await expect(page.getByTestId("inference-current-provider")).toHaveText(
    "Managed (TinyHumans)",
    { timeout: 30_000 },
  );
  await expect(page.locator("#inference-provider")).toHaveText(/Managed \(TinyHumans\)/);

  // And it is in the list, so it can be switched *to* as well as reported.
  await page.locator("#inference-provider").click();
  await expect(page.getByRole("option", { name: "OpenRouter", exact: true })).toBeVisible();
  await expect(
    page.getByRole("option", { name: "Managed (TinyHumans)", exact: true }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
});

test("a key typed for a BYOK provider is not discarded by switching provider", async ({
  page,
}) => {
  await openConnections(page);

  // Since #585 the key input is offered for every provider but Ollama — with
  // the line that says what paying for the company actually means.
  await expect(page.locator("#inference-key")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("inference-key-note")).toBeVisible();

  // Type a key under one provider, then switch to another and back. The value
  // survives the switch — that is the state that used to lose it. The pair
  // used to be OpenRouter and managed, back when managed was not selectable;
  // Custom stands in for it here instead, but the defect was never about
  // *which* two providers, only about crossing between any of them.
  await pickProvider(page, "OpenRouter");
  const typed = `pw-e2e-${Date.now()}`;
  await page.locator("#inference-key").fill(typed);
  await pickProvider(page, "Custom (OpenAI-compatible)");
  await expect(page.locator("#inference-key")).toHaveValue(typed);
  await pickProvider(page, "OpenRouter");
  await expect(page.locator("#inference-key")).toHaveValue(typed);

  // Saving now stores it rather than reverting past it. The credential is
  // write-only, so `keyConfigured` is the only observable — run this against a
  // fresh `--home` for it to mean "this save stored it".
  await page.getByTestId("inference-save").click();
  await expect(
    page.getByText(/Inference updated\.|Inference saved — restart the company/),
  ).toBeVisible({ timeout: 30_000 });

  const after = await page.request.get("/api/v1/company/inference");
  expect(after.ok()).toBeTruthy();
  const body = await after.json();
  expect(body.keyConfigured).toBe(true);
  // Setting a key under OpenRouter lands on OpenRouter — which is also what the
  // legacy `managed` alias normalizes to, so this stays the same assertion it
  // was when the switch above ended on managed.
  expect(body.provider).toBe("openrouter");

  // And it can be taken back off again — set / rotate / clear, all from here.
  await page.getByTestId("inference-remove-key").click();
  await expect(page.getByText("Removed the company key.")).toBeVisible({ timeout: 30_000 });
  const cleared = await page.request.get("/api/v1/company/inference");
  expect((await cleared.json()).keyConfigured).toBe(false);
});

test("a managed company with its own key names the managed route in both places", async ({
  page,
}) => {
  // The regression this exists for: saving the managed card with a TinyHumans
  // key sent every turn to openrouter.ai, and the card then read `OpenRouter`
  // in the header while the endpoint line still said the platform. The probe
  // resolved on the raw provider kind and runtime resolution normalized
  // `managed` to `openrouter` first, so onboarding verified a configuration no
  // turn could use.
  //
  // Driven with a key set, because managed WITHOUT a key was never broken — the
  // keyless inheritance arm caught it. The key is what skipped that arm.
  await openConnections(page);
  await expect(page.locator("#inference-key")).toBeVisible({ timeout: 30_000 });

  await pickProvider(page, "Managed (TinyHumans)");
  await page.locator("#inference-key").fill(`pw-e2e-managed-${Date.now()}`);
  await page.getByTestId("inference-save").click();
  await expect(
    page.getByText(/Inference updated\.|Inference saved — restart the company/),
  ).toBeVisible({ timeout: 30_000 });

  // The host is the authority on which route the traffic takes.
  const body = await (await page.request.get("/api/v1/company/inference")).json();
  expect(body.keyConfigured).toBe(true);
  expect(body.baseUrl).not.toContain("openrouter.ai");
  expect(body.slug).toBe("subscription");

  // And the card agrees with it, in both the places that name a provider —
  // they used to disagree, which is what made the defect look like a display
  // glitch rather than a routing one.
  await expect(page.getByTestId("inference-current-provider")).toHaveText(
    "Managed (TinyHumans)",
    { timeout: 30_000 },
  );
  await expect(page.locator("#inference-provider")).toHaveText(/Managed \(TinyHumans\)/);

  // Nothing is pending straight after a save, so the unsaved cue must be absent
  // — otherwise it would cry wolf on every load and stop meaning anything.
  await expect(page.getByTestId("inference-unsaved")).toHaveCount(0);

  // Leave the company on its committed default for whatever runs next.
  await page.getByRole("button", { name: "Reset to default" }).click();
  await expect(
    page.getByText("Reverted to the committed manifest (or managed) configuration."),
  ).toBeVisible({ timeout: 30_000 });
});

test("an edited form says it is unsaved while the header keeps reporting the running config", async ({
  page,
}) => {
  // The header states what the company runs; the form states what it would
  // switch to. They legitimately differ mid-edit, and without a cue that gap is
  // indistinguishable from the card contradicting itself — which it did, for
  // real, until the fix above.
  await openConnections(page);
  await expect(page.locator("#inference-key")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("inference-unsaved")).toHaveCount(0);

  await pickProvider(page, "OpenRouter");

  await expect(page.getByTestId("inference-unsaved")).toBeVisible();
  // The header is unmoved: nothing was saved, so nothing about the running
  // company changed.
  await expect(page.getByTestId("inference-current-provider")).toHaveText(
    "Managed (TinyHumans)",
  );
});

test("a key typed for a BYOK provider does reach the host on save", async ({ page }) => {
  // The managed case above must not be the only one that lands: the same input,
  // saved under a provider with its own endpoint, still has to reach the host.
  await openConnections(page);
  await expect(page.locator("#inference-key")).toBeVisible({ timeout: 30_000 });

  await pickProvider(page, "Custom (OpenAI-compatible)");
  await page.locator("#inference-base-url").fill("http://127.0.0.1:9/v1");
  // The model grid moved to its own "Manage Routing" tab (InferenceView's
  // own tab split, the same shape AgentDetailView and MemoryView already
  // have) -- Connect and Manage Routing are one draft and one Save, but two
  // tab panels, so filling both fields means visiting both.
  await page.getByRole("tab", { name: "Manage Routing" }).click();
  await page.locator("#inference-model-chat-v1").fill("pw-e2e-model");
  await page.getByRole("tab", { name: "Connect" }).click();
  await page.locator("#inference-key").fill(`pw-e2e-${Date.now()}`);
  await page.getByTestId("inference-save").click();

  // Either success wording is correct here, and which one shows is not this
  // spec's business: a company that booted with no inference source is on the
  // echo brain, so issue #266 makes the host report `restartRequired` for
  // exactly this not-configured → configured save and the toast says "restart"
  // instead of "next turn". What #265 asserts is that the save was *accepted*
  // and the key kept — the stored-credential check below is the real proof.
  await expect(
    page.getByText(/Inference updated\.|Inference saved — restart the company/),
  ).toBeVisible({ timeout: 30_000 });
  const status = await page.request.get("/api/v1/company/inference");
  expect(status.ok()).toBeTruthy();
  const body = await status.json();
  expect(body.keyConfigured).toBe(true);
  expect(body.provider).toBe("openai_compatible");

  // Put the company back on the committed default for whatever runs next.
  await page.getByRole("button", { name: "Reset to default" }).click();
  await expect(
    page.getByText("Reverted to the committed manifest (or managed) configuration."),
  ).toBeVisible({ timeout: 30_000 });

  // The reset is a full one, not a half-clear: the host also wipes the stored
  // credential on revert (issue #993), so nothing is left behind to reroute the
  // later specs in this lane (the live-brain workflow and MCP-agent specs) off
  // the mock brain and 401 them. Assert that here rather than clearing by hand
  // — the remove-key button exists only while a key is stored, so it being gone
  // is the observable that the reset actually cleared the key.
  await expect(page.getByTestId("inference-remove-key")).toHaveCount(0, {
    timeout: 30_000,
  });
  const cleared = await page.request.get("/api/v1/company/inference");
  expect((await cleared.json()).keyConfigured).toBe(false);
});

test("changing provider asks before replacing a typed endpoint or model", async ({ page }) => {
  await openConnections(page);
  await expect(page.locator("#inference-key")).toBeVisible({ timeout: 30_000 });

  await pickProvider(page, "Custom (OpenAI-compatible)");
  await page.locator("#inference-base-url").fill("https://models.example.test/v1");
  // The model grid is on its own "Manage Routing" tab; the provider select
  // that triggers the replace-warning dialog is back on Connect.
  await page.getByRole("tab", { name: "Manage Routing" }).click();
  await page.locator("#inference-model-chat-v1").fill("operator-draft");
  await page.getByRole("tab", { name: "Connect" }).click();
  await pickProvider(page, "OpenRouter");

  await expect(page.getByRole("alertdialog")).toContainText("replaces the typed Base URL and model fields");
  await page.getByRole("button", { name: "Keep draft" }).click();
  await expect(page.locator("#inference-provider")).toContainText("Custom (OpenAI-compatible)");
  await expect(page.locator("#inference-base-url")).toHaveValue("https://models.example.test/v1");
  await page.getByRole("tab", { name: "Manage Routing" }).click();
  await expect(page.locator("#inference-model-chat-v1")).toHaveValue("operator-draft");
});

test("OpenRouter models are selected from the registry and persist through reload", async ({
  page,
}) => {
  await page.route("**/inference/models", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      // `GET …/inference/models` answers with the configured endpoint's own
      // catalog — `{baseUrl, models, tierVocabulary, tierDefaults}` — not the
      // bare array it used to return. These ids are neither the tier names nor
      // the shipped concrete ids, so a real host classifies this endpoint
      // `unknown` and supplies no tier defaults; the console then keeps
      // prefilling from `status.defaultTierModels`, as it does here.
      body: JSON.stringify({
        baseUrl: "https://catalog.example.test/v1",
        models: [
          { id: "provider/catalog-chat", name: "Catalog Chat", contextLength: 128_000 },
          { id: "provider/catalog-reasoning", name: "Catalog Reasoning" },
        ],
        tierVocabulary: "unknown",
        tierDefaults: {},
      }),
    });
  });
  await openConnections(page);
  await expect(page.locator("#inference-provider")).toBeVisible({ timeout: 30_000 });

  await pickProvider(page, "OpenRouter");
  // A key is required before the catalog picker offers itself (issue #1838
  // follow-up): with none typed and no key already stored server-side, a
  // save would ride the platform's subscription proxy, which rejects the
  // raw `<author>/<model>` id the catalog select writes — so the free-text
  // inputs stay up instead until a key makes that pick safe to store. This
  // spec runs against the shared E2E company, whose key state a prior spec
  // in this file (or an earlier run of this one) can have left cleared, so
  // typing one here is what makes the picker's availability deterministic
  // rather than an accident of what state a previous test left behind.
  await page.locator("#inference-key").fill(`pw-e2e-${Date.now()}`);
  // The catalog picker lives on Manage Routing, not Connect.
  await page.getByRole("tab", { name: "Manage Routing" }).click();
  const chat = page.getByTestId("inference-model-select-chat-v1");
  await expect(chat).toBeEnabled();
  await chat.click();
  await page.getByRole("option", { name: /Catalog Chat/ }).click();
  await page.getByTestId("inference-save").click();
  await expect(
    page.getByText(/Inference updated\.|Inference saved — restart the company/),
  ).toBeVisible({ timeout: 30_000 });

  const saved = await page.request.get("/api/v1/company/inference");
  expect(saved.ok()).toBeTruthy();
  expect((await saved.json()).models["chat-v1"]).toBe("provider/catalog-chat");

  await page.reload();
  // `tab` rides the hash (useHashTab), so a reload keeps Manage Routing
  // selected -- clicked again here anyway, to assert the tab explicitly
  // rather than lean on that persistence.
  await page.getByRole("tab", { name: "Manage Routing" }).click();
  await expect(page.getByTestId("inference-model-select-chat-v1")).toContainText("Catalog Chat", {
    timeout: 30_000,
  });

  await page.getByRole("tab", { name: "Connect" }).click();
  await pickProvider(page, "Custom (OpenAI-compatible)");
  await page.getByRole("tab", { name: "Manage Routing" }).click();
  await expect(page.locator("input#inference-model-chat-v1")).toBeVisible();

  // Leave the shared E2E company on its committed default for later specs.
  await page.getByRole("button", { name: "Reset to default" }).click();
  await expect(
    page.getByText("Reverted to the committed manifest (or managed) configuration."),
  ).toBeVisible({ timeout: 30_000 });
});

test("a saved OpenRouter tier override can be cleared back to the tier default (issue #1838 follow-up)", async ({
  page,
}) => {
  // Once a keyed company has picked a concrete model for a tier, the select
  // used to offer no way back — every option only replaced the override, and
  // Reset throws away the whole provider configuration and key rather than
  // one tier's mapping. This proves the explicit "Use the tier default" item
  // actually clears the stored override, end to end against a real host.
  await page.route("**/inference/models", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      // Same catalog shape as the spec above: an object, not a bare array.
      body: JSON.stringify({
        baseUrl: "https://catalog.example.test/v1",
        models: [{ id: "provider/catalog-chat", name: "Catalog Chat", contextLength: 128_000 }],
        tierVocabulary: "unknown",
        tierDefaults: {},
      }),
    });
  });
  await openConnections(page);
  await expect(page.locator("#inference-provider")).toBeVisible({ timeout: 30_000 });

  await pickProvider(page, "OpenRouter");
  await page.locator("#inference-key").fill(`pw-e2e-${Date.now()}`);
  // The catalog picker lives on Manage Routing, not Connect.
  await page.getByRole("tab", { name: "Manage Routing" }).click();
  const chat = page.getByTestId("inference-model-select-chat-v1");
  await expect(chat).toBeEnabled();
  await chat.click();
  await page.getByRole("option", { name: /Catalog Chat/ }).click();
  await page.getByTestId("inference-save").click();
  await expect(
    page.getByText(/Inference updated\.|Inference saved — restart the company/),
  ).toBeVisible({ timeout: 30_000 });

  const saved = await page.request.get("/api/v1/company/inference");
  expect(saved.ok()).toBeTruthy();
  expect((await saved.json()).models["chat-v1"]).toBe("provider/catalog-chat");

  // Reload so the picker is seeded straight from the stored override, then
  // clear it through the select rather than typing anything. `tab` rides the
  // hash (useHashTab), so this reload keeps Manage Routing selected --
  // clicked again here anyway, to assert the tab explicitly rather than lean
  // on that persistence.
  await page.reload();
  await page.getByRole("tab", { name: "Manage Routing" }).click();
  const chatAfterReload = page.getByTestId("inference-model-select-chat-v1");
  await expect(chatAfterReload).toContainText("Catalog Chat", { timeout: 30_000 });
  await chatAfterReload.click();
  await page.getByRole("option", { name: "Use the tier default" }).click();
  await page.getByTestId("inference-save").click();
  await expect(
    page.getByText(/Inference updated\.|Inference saved — restart the company/),
  ).toBeVisible({ timeout: 30_000 });

  const cleared = await page.request.get("/api/v1/company/inference");
  expect(cleared.ok()).toBeTruthy();
  expect((await cleared.json()).models["chat-v1"]).toBeUndefined();

  // Leave the shared E2E company on its committed default for later specs.
  await page.getByRole("button", { name: "Reset to default" }).click();
  await expect(
    page.getByText("Reverted to the committed manifest (or managed) configuration."),
  ).toBeVisible({ timeout: 30_000 });
});

test("typing an OpenRouter passthrough id one keystroke at a time is not stripped mid-word (issue #1838 follow-up, sixth instance)", async ({
  page,
}) => {
  // Unit coverage for this same regression (inference-model-picker.test.ts)
  // dispatches synthetic input events; this spec is the proof against a real
  // browser's actual keystroke-by-keystroke typing, which is what a prior
  // regression on this same effect (the baseline/models divergence, #1838)
  // slipped past unit tests and only an E2E run caught.
  await openConnections(page);
  await expect(page.locator("#inference-key")).toBeVisible({ timeout: 30_000 });

  // A keyless switch to OpenRouter lands on the proxied, free-text path
  // (issue #1838 follow-up): the provider's own raw-id presets get stripped
  // immediately since there is no key to save them under, leaving the tier
  // fields editable and empty.
  //
  // Make that keyless precondition explicit rather than inherited from
  // whatever a prior spec (or an aborted earlier run) left stored on the
  // shared E2E company: the free-text `<input>` and the catalog `Select`
  // trigger share the same `id={inference-model-${tier}}` (only one renders
  // at a time), so `#inference-model-chat-v1` alone would just as happily
  // match a leftover-key company's Select trigger — and `toHaveValue("")`
  // against that gives a confusing "not an input" failure instead of naming
  // the real problem. `input#…` only matches the free-text control, the same
  // guard the OpenRouter-catalog spec above already uses.
  await pickProvider(page, "OpenRouter");
  // The free-text/catalog model control lives on Manage Routing, not Connect.
  await page.getByRole("tab", { name: "Manage Routing" }).click();
  const chatInput = page.locator("input#inference-model-chat-v1");
  await expect(chatInput).toBeVisible();
  await expect(chatInput).toHaveValue("");

  // Type the proxy's own passthrough id character by character. Every prefix
  // shorter than the full three-segment id counts as a raw catalog id by
  // segment count, so this is exactly the sequence a per-render strip used to
  // clear mid-word.
  const target = "openrouter/anthropic/claude-sonnet-5";
  await chatInput.pressSequentially(target, { delay: 20 });
  await expect(chatInput).toHaveValue(target);

  // Leave the shared E2E company on its committed default for later specs.
  await page.getByRole("button", { name: "Reset to default" }).click();
  await expect(
    page.getByText("Reverted to the committed manifest (or managed) configuration."),
  ).toBeVisible({ timeout: 30_000 });
});
