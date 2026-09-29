import { afterAll, describe, expect, test, vi } from "vitest";

const ENV_NAMES = [
  "PW_ANALYTICS",
  "PW_ANALYTICS_HOST_BIND",
  "PW_BASE_URL",
  "PW_HOST_BIND",
  "PW_HOST_DATA_DIR",
] as const;
const originalEnv = Object.fromEntries(
  ENV_NAMES.map((name) => [name, process.env[name]]),
);

afterAll(() => {
  for (const name of ENV_NAMES) {
    const value = originalEnv[name];
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
});

async function loadConfig(analytics: boolean) {
  for (const name of ENV_NAMES) delete process.env[name];
  if (analytics) process.env.PW_ANALYTICS = "1";
  vi.resetModules();
  return (await import("../../playwright.config")).default;
}

function hostServer(config: Awaited<ReturnType<typeof loadConfig>>) {
  const servers = Array.isArray(config.webServer)
    ? config.webServer
    : [config.webServer];
  return servers.at(-1);
}

describe("Playwright analytics lane", () => {
  test("keeps the ordinary lane silent on its ordinary host", async () => {
    const config = await loadConfig(false);
    const host = hostServer(config);

    expect(config.use?.baseURL).toMatch(/^http:\/\/127\.0\.0\.1:\d+$/);
    expect(config.use?.storageState).toMatch(
      /target\/e2e\/storage-state\.json$/,
    );
    expect(config.testIgnore).not.toContainEqual(
      /opencompany-config\.spec\.ts$/,
    );
    expect(host?.env).not.toHaveProperty("OPENCOMPANY_ANALYTICS");
    expect(host?.env?.PW_HOST_DATA_DIR).toBeUndefined();
  });

  test("selects the config spec on an isolated opted-in host", async () => {
    const config = await loadConfig(true);
    const host = hostServer(config);
    const ordinaryBaseURL = (await loadConfig(false)).use?.baseURL;

    expect(config.use?.baseURL).toMatch(/^http:\/\/127\.0\.0\.1:\d+$/);
    expect(config.use?.baseURL).not.toBe(ordinaryBaseURL);
    expect(config.use?.storageState).toMatch(
      /target\/e2e\/analytics-storage-state\.json$/,
    );
    expect(config.testMatch).toEqual(/opencompany-config\.spec\.ts$/);
    expect(host?.env).toMatchObject({
      OPENCOMPANY_ANALYTICS: "on",
      OPENCOMPANY_ANALYTICS_ENDPOINT: "https://collector.example/api/track",
      PW_HOST_DATA_DIR: expect.stringMatching(/target\/e2e\/analytics-data$/),
    });
  });
});
