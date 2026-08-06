import { defineConfig } from "vitest/config";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

/**
 * The console's unit-test runner (issue #434).
 *
 * # What belongs here, and what belongs in `test/e2e/`
 *
 * This runner is for **pure functions**: a helper that maps A to B with no
 * document, no host, and no React. `test/e2e/` is for everything that is only
 * true in a browser driving a live host — a disabled affordance explaining
 * itself, a banner that must not be a toast, a redirect that survives a
 * full-page navigation.
 *
 * The line matters because the two are tempted to test each other's territory.
 * A browser walk *can* reach a pure helper, through six layers of render, in
 * forty seconds, and it will report the failure as "the board looked wrong".
 * A unit test cannot reach a redirect at all. Put a helper here the moment it
 * has a second caller or a branch worth naming; leave anything that needs a
 * document over there.
 *
 * # Why this is its own config and not `vite.config.ts`
 *
 * `vite.config.ts` loads the React and Tailwind plugins, and this suite needs
 * neither — it imports TypeScript modules and calls them. Keeping the config
 * separate is what keeps the run in the low hundreds of milliseconds, which is
 * the property issue #434 actually asked for: a gate slow enough to skip gets
 * skipped, and a skipped gate is the problem it was filed about.
 *
 * The `@` alias is therefore redeclared here rather than inherited. It is the
 * one thing this config shares with the app build, and it must not drift from
 * `vite.config.ts` / `tsconfig.app.json`.
 */
export default defineConfig({
  resolve: {
    alias: {
      "@": resolve(here, "./src"),
    },
  },
  test: {
    // `node` by DEFAULT, jsdom only where a test says so with a
    // `@vitest-environment jsdom` docblock. Standing up a DOM costs more than
    // every pure test in this suite put together, and most of them have no use
    // for one.
    environment: "node",
    // Scoped to `test/unit`, so a Playwright spec under `test/e2e` — which
    // imports `@playwright/test` and needs a browser — can never be collected
    // by this runner.
    include: ["test/unit/**/*.test.ts"],
  },
});
