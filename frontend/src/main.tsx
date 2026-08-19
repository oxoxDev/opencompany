import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { ThemeProvider } from "next-themes";

import { App } from "./App";
import { TooltipProvider } from "@/components/ui/tooltip";
import { Toaster } from "@/components/ui/sonner";
import { startScrollActivity } from "@/lib/scroll-activity";
import "./index.css";

/**
 * The desktop needs no pre-mount step.
 *
 * There used to be one here: it invoked a `desktop_config` command, redeemed a
 * `dev_code` and reloaded the page with `?api=&code=`. That command is not in
 * this build's `generate_handler!`, so it always threw and the `catch` swallowed
 * it — and its `fetch` straight out of the webview is what the Rust proxy and
 * the CSP both exist to avoid. The desktop now boots like any other console and
 * discovers its hosts through the connection registry (see `App`).
 */
function mount(): void {
  const root = document.getElementById("root");
  if (!root) throw new Error("missing #root element");
  createRoot(root).render(
    <StrictMode>
      <ThemeProvider attribute="class" defaultTheme="system" enableSystem disableTransitionOnChange>
        <TooltipProvider delay={200}>
          <App />
          <Toaster position="bottom-right" richColors closeButton />
        </TooltipProvider>
      </ThemeProvider>
    </StrictMode>,
  );
}

// Started before the first render and never disposed: it is one capturing
// listener for the life of the document, and every scroll container in the app
// — including ones mounted much later — depends on it for the `data-scrolling`
// mark the themed scrollbars in `index.css` lift on. Outside React on purpose,
// so StrictMode's double-invoked effects cannot arm it twice.
startScrollActivity();

mount();
