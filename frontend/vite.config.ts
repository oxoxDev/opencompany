import { readFileSync } from "node:fs";
import path from "node:path";

import { defineConfig, type PluginOption } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { sentryVitePlugin } from "@sentry/vite-plugin";

// The console is company-agnostic and talks to the OpenCompany operator API.
// In dev it proxies the API routes to a locally-running `opencompany serve`
// (default 127.0.0.1:8080), so the app is same-origin and needs no CORS.
// Override the target with OC_API_TARGET when the host runs elsewhere.
const API_TARGET = process.env.OC_API_TARGET ?? "http://127.0.0.1:8080";

// Read rather than imported: `tsconfig.node.json` does not enable
// `resolveJsonModule`, and turning it on to learn one string is a wider change
// than reading the file.
const { version } = JSON.parse(
  readFileSync(path.resolve(__dirname, "package.json"), "utf8"),
) as { version: string };

/**
 * The Sentry release tag, computed once and used twice.
 *
 * Shaped to match the host's `observability::config::release_tag`
 * (`opencompany@<version>[+<commit>]`), so a console event and a host event
 * from the same build carry the same release string whether the operator files
 * them under one Sentry project or two.
 *
 * `SENTRY_RELEASE` wins outright, for a CI that already knows what it is
 * shipping. Otherwise `VITE_BUILD_COMMIT` — the frontend's spelling of the
 * host's `OPENCOMPANY_BUILD_COMMIT` — supplies the commit, shortened to the
 * twelve characters `src/build_stamp.rs` normalizes to. With neither, the
 * version alone: an honest "this build cannot say which commit it is" beats a
 * release name that looks like a commit and is not one.
 */
function sentryRelease(): string {
  const explicit = (process.env.SENTRY_RELEASE ?? "").trim();
  if (explicit) return explicit;
  const commit = (process.env.VITE_BUILD_COMMIT ?? "").trim().slice(0, 12);
  return commit ? `opencompany@${version}+${commit}` : `opencompany@${version}`;
}

const RELEASE = sentryRelease();

/**
 * Source-map upload, or nothing at all.
 *
 * Gated on `SENTRY_AUTH_TOKEN`, a CI secret that is absent in every local
 * checkout — so `npm run build` on a laptop skips this entirely and needs no
 * Sentry account, no org and no project. That gate is also why `sourcemap`
 * below is tied to the same condition: this plugin deletes the `.map` files
 * after uploading them, so emitting maps unconditionally would ship the
 * console's source to every viewer of any build made without a token.
 *
 * `url` matters for a self-hosted Sentry. Without it the plugin defaults to
 * sentry.io, where the upload lands somewhere the events never will.
 */
function sentrySourceMapUpload(): PluginOption | null {
  const authToken = (process.env.SENTRY_AUTH_TOKEN ?? "").trim();
  if (!authToken) return null;
  return sentryVitePlugin({
    authToken,
    url: process.env.SENTRY_URL,
    org: process.env.SENTRY_ORG,
    project: process.env.SENTRY_PROJECT,
    // The plugin otherwise reports an upload error but lets Vite exit zero.
    // A release whose stack traces cannot be symbolicated is not complete.
    errorHandler(error) {
      throw error;
    },
    release: {
      name: RELEASE,
      // The bundle already carries this release through the
      // `__SENTRY_RELEASE__` define, so the plugin's virtual release module
      // would be a second injection point for a value that is already there.
      inject: false,
    },
    sourcemaps: {
      // Absolute, anchored on this file's directory. The plugin resolves these
      // globs against `process.cwd()` rather than against Vite's root, so a
      // relative glob silently matches nothing whenever the build is started
      // from somewhere else — `npm --prefix frontend run build`, the
      // Dockerfile, `scripts/desktop-dev.sh`. The only symptom is a
      // "Didn't find any matching sources for debug ID upload" line in a log
      // nobody reads, and un-symbolicated stack traces weeks later.
      assets: [
        path.resolve(__dirname, "dist/**/*.js"),
        path.resolve(__dirname, "dist/**/*.map"),
      ],
      // Never ship raw maps to a browser: the upload keeps a copy server-side
      // for symbolication and the bundle goes out without them.
      filesToDeleteAfterUpload: [path.resolve(__dirname, "dist/**/*.map")],
    },
    telemetry: false,
  });
}

const sentryPlugin = sentrySourceMapUpload();

export default defineConfig({
  plugins: [react(), tailwindcss(), sentryPlugin].filter(
    Boolean,
  ) as PluginOption[],
  define: {
    // One release string for the bundle and for the upload. See
    // `src/vite-env.d.ts` for why this is a define and not a `VITE_*`.
    __SENTRY_RELEASE__: JSON.stringify(RELEASE),
  },
  build: {
    // Emitted only when something is going to upload and then delete them.
    sourcemap: sentryPlugin !== null,
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
    proxy: {
      "/api": { target: API_TARGET, changeOrigin: true },
      "/healthz": { target: API_TARGET, changeOrigin: true },
      "/spec": { target: API_TARGET, changeOrigin: true },
      "/tiny": { target: API_TARGET, changeOrigin: true },
      // The pages SDK bundle (`@opencompany/site` + the bundled React the
      // served page import map resolves "react"/"react-dom/client" to) isn't
      // under `/api` — it's served straight off the console's static dir at
      // `OPENCOMPANY_CONSOLE_DIR` (see `frontend/pages-sdk/`). `{scope}/pages`
      // itself needs no separate entry here: `{scope}` is already an `/api`
      // path, so it rides the `/api` proxy above.
      "/pages-sdk": { target: API_TARGET, changeOrigin: true },
    },
  },
});
