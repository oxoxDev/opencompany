import path from "node:path";

import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Builds `@opencompany/site` — the component subset + postMessage `client`
// every agent-authored page imports — to `dist/pages-sdk/index.mjs` plus its
// compiled CSS. Reuses the console's own `@/*` alias so the re-exported
// `src/components/ui/*` files and `src/index.css` need no edits to build
// here too.
//
// React/ReactDOM stay external: the host serves an import map that resolves
// "react" and "react-dom/client" to a separately-built bundle (`react.mjs`,
// from `vite.react.config.ts`), so a page's module graph never pulls in a
// second copy of React alongside the one the import map already provides.
export default defineConfig({
  define: {
    "process.env.NODE_ENV": '"production"',
  },
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "../src"),
    },
  },
  build: {
    outDir: path.resolve(__dirname, "../dist/pages-sdk"),
    // The react.mjs build (a separate `vite build` invocation) writes into
    // this same directory; each must leave the other's output alone.
    emptyOutDir: false,
    cssMinify: true,
    // Library mode defaults `minify` to `false` (unlike a regular app
    // build) — explicit here so what ships to a sandboxed page isn't an
    // unminified dev-sized bundle.
    minify: "esbuild",
    lib: {
      entry: path.resolve(__dirname, "index.ts"),
      formats: ["es"],
      fileName: () => "index.mjs",
    },
    rollupOptions: {
      external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime"],
      output: {
        // Vite's library CSS output otherwise names itself after the
        // package (`style.css`) — pin it to what the served import map and
        // Docker COPY expect.
        assetFileNames: "index[extname]",
      },
    },
  },
});
