import { expect, test } from "@playwright/test";

test("serves the runtime console configuration before OpenPanel loads", async ({
  page,
  request,
}) => {
  test.skip(
    Boolean(process.env.PW_BASE_URL),
    "requires the Playwright-managed host configuration",
  );
  let openPanelLoaderRequested = false;
  await page.route("https://openpanel.dev/op1.js", async (route) => {
    openPanelLoaderRequested = true;
    await route.fulfill({ contentType: "application/javascript", body: "" });
  });
  const [indexResponse, configResponse] = await Promise.all([
    request.get("/"),
    request.get("/opencompany-config.js"),
  ]);

  expect(indexResponse.ok()).toBe(true);
  expect(await indexResponse.text()).toContain(
    '<script src="/opencompany-config.js" defer></script>',
  );

  expect(configResponse.ok()).toBe(true);
  expect(configResponse.headers()["content-type"]).toContain(
    "application/javascript",
  );
  expect(configResponse.headers()["cache-control"]).toBe("no-store");
  const expectedConfig =
    process.env.PW_ANALYTICS === "1"
      ? "window.OPENCOMPANY_CONFIG=Object.assign(window.OPENCOMPANY_CONFIG||{}," +
        '{analytics:true,analyticsEndpoint:"https://collector.example/api"});\n'
      : "window.OPENCOMPANY_CONFIG=window.OPENCOMPANY_CONFIG||{};\n";
  expect(await configResponse.text()).toBe(expectedConfig);

  await page.goto("/");
  await expect(page).toHaveTitle("OpenCompany Console");
  if (process.env.PW_ANALYTICS === "1") {
    await expect.poll(() => openPanelLoaderRequested).toBe(true);
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as { op?: { q?: unknown[] } }).op?.q,
        ),
      )
      .toContainEqual([
        "init",
        expect.objectContaining({
          apiUrl: "https://collector.example/api",
          clientId: "afe8ec4e-0a6a-427a-aa22-49cbbf137d0a",
        }),
      ]);
  } else {
    expect(openPanelLoaderRequested).toBe(false);
    expect(
      await page.evaluate(() => (window as unknown as { op?: unknown }).op),
    ).toBeUndefined();
  }
});
