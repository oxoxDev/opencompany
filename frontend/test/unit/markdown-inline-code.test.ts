// @vitest-environment jsdom

import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { compile } from "tailwindcss";
import { beforeAll, beforeEach, afterEach, describe, expect, it } from "vitest";

import { Markdown } from "@/components/markdown";

/**
 * Inline code must not render literal backticks (issue #1108).
 *
 * `@tailwindcss/typography` styles hand-written HTML, where nothing else marks
 * a `code` span, so it draws a backtick before and after every one of them.
 * `components/markdown.tsx` feeds it the *output* of a markdown parser, where
 * the fences were already consumed — so the console rendered `` `UsageDto` ``
 * everywhere an agent named a type. `src/index.css` nulls that content.
 *
 * # Why this test compiles CSS instead of reading the DOM
 *
 * The backticks are `::before`/`::after` content. They never exist as nodes,
 * so no amount of rendering can see them: jsdom does not implement
 * `getComputedStyle(el, "::before")` at all, and the `<code>` element's own
 * `textContent` is identical before and after the fix. Asserting on the
 * rendered text would pass against the bug.
 *
 * What decides the question is the compiled stylesheet, so that is what this
 * builds — with the real plugin and the real `src/index.css` — and then
 * resolves against the real rendered document. That covers the two ways this
 * regresses: the override being deleted, and a plugin upgrade moving the
 * default somewhere the override no longer outranks.
 */

const require = createRequire(import.meta.url);
const frontendRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

/** One flattened CSS rule: a full selector, and where the cascade ranks it. */
type Rule = {
  selector: string;
  declarations: Record<string, string>;
  /** 1 for an unlayered rule, 0 for one inside any `@layer`. See `winner`. */
  unlayered: number;
  order: number;
};

/**
 * Flattens the compiled stylesheet into a flat, ordered rule list.
 *
 * Tailwind v4 emits native nesting (`.prose { :where(code) { … } }`) wrapped in
 * `@layer` blocks, and neither jsdom's CSSOM nor `document.styleSheets` parses
 * that. This walks the braces instead: a nested selector is joined to its
 * parent as a descendant (which is what every rule here is), an
 * `@layer`/`@media`/`@supports` block is descended into without contributing a
 * selector, and any other at-rule (`@property`, `@keyframes`) is skipped whole.
 */
function flatten(css: string): Rule[] {
  const rules: Rule[] = [];

  function walk(body: string, parent: string, unlayered: number, into?: Rule) {
    let head = "";
    let i = 0;
    while (i < body.length) {
      const ch = body[i];
      if (ch === ";") {
        // At this depth a `;` ends a declaration of the block being walked.
        const at = head.indexOf(":");
        if (into && at > 0) into.declarations[head.slice(0, at).trim()] = head.slice(at + 1).trim();
        head = "";
        i += 1;
        continue;
      }
      if (ch !== "{") {
        head += ch;
        i += 1;
        continue;
      }
      const from = i + 1;
      let depth = 1;
      i += 1;
      while (i < body.length && depth > 0) {
        if (body[i] === "{") depth += 1;
        else if (body[i] === "}") depth -= 1;
        i += 1;
      }
      const inner = body.slice(from, i - 1);
      const header = head.trim();
      head = "";
      if (header.startsWith("@")) {
        if (/^@(layer|media|supports|container)\b/.test(header)) {
          walk(inner, parent, header.startsWith("@layer") ? 0 : unlayered, into);
        }
        continue;
      }
      const rule: Rule = {
        selector: parent ? `${parent} ${header}` : header,
        declarations: {},
        unlayered,
        order: rules.length,
      };
      rules.push(rule);
      walk(inner, rule.selector, unlayered, rule);
    }
  }

  walk(css, "", 1);
  return rules;
}

/**
 * Splits a selector list on its top-level commas only.
 *
 * `String.split(",")` cannot be used: every selector the typography plugin
 * emits carries a `:not(:where(a, b))` whose own comma would tear it in half,
 * and the halves are not valid selectors.
 */
function splitSelectorList(selector: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let current = "";
  for (const ch of selector) {
    if (ch === "(") depth += 1;
    else if (ch === ")") depth -= 1;
    if (ch === "," && depth === 0) {
      out.push(current);
      current = "";
      continue;
    }
    current += ch;
  }
  out.push(current);
  return out.map((one) => one.trim()).filter(Boolean);
}

/**
 * The declaration that actually paints, for one element and one property.
 *
 * Ranked by cascade layer first, then source order — deliberately *not* by
 * specificity, because every rule in play here is `:where()`-wrapped to the
 * single `.prose` class, so specificity is a tie by construction and the layer
 * is what decides. A future rule that broke that assumption would be visible
 * in the same compiled CSS this reads.
 */
function winner(rules: Rule[], element: Element, property: string, pseudo = ""): string | undefined {
  const matched = rules
    .filter(
      (rule) =>
        property in rule.declarations &&
        splitSelectorList(rule.selector).some((one) => {
          const own = one.match(/::[a-z-]+$/)?.[0] ?? "";
          if (own !== pseudo) return false;
          try {
            return element.matches(pseudo ? one.slice(0, -pseudo.length) : one);
          } catch {
            return false;
          }
        }),
    )
    .sort((a, b) => a.unlayered - b.unlayered || a.order - b.order);
  return matched.at(-1)?.declarations[property];
}

/**
 * Resolves one `@import` target to a file on disk.
 *
 * Node cannot do this alone. A CSS-only package names its stylesheet under the
 * `style` export condition (`tw-animate-css`, `shadcn/tailwind.css`), which
 * `require.resolve` does not read — and several of them do not export
 * `./package.json` either, so the manifest has to be found by walking
 * `node_modules` rather than resolved. `"tailwindcss"` is special-cased
 * because it resolves to the plugin's JS, not to its stylesheet.
 */
function resolveStylesheet(id: string, base: string): string {
  if (id === "tailwindcss") return require.resolve("tailwindcss/index.css");
  try {
    return require.resolve(id, { paths: [base, frontendRoot] });
  } catch {
    const parts = id.split("/");
    const name = parts.slice(0, id.startsWith("@") ? 2 : 1).join("/");
    const subpath = parts.slice(id.startsWith("@") ? 2 : 1).join("/");
    const manifestPath = findManifest(name, base);
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    const entry = manifest.exports?.[subpath ? `./${subpath}` : "."];
    const file =
      (typeof entry === "string" ? entry : entry?.style) ?? manifest.style ?? manifest.main;
    if (!file) throw new Error(`no stylesheet entry for "${id}"`);
    return resolve(dirname(manifestPath), file);
  }
}

/** Walks up from `base` for `node_modules/<name>/package.json`. */
function findManifest(name: string, base: string): string {
  for (let dir = base; ; dir = dirname(dir)) {
    const candidate = resolve(dir, "node_modules", name, "package.json");
    if (existsSync(candidate)) return candidate;
    if (dirname(dir) === dir) throw new Error(`cannot find package "${name}" from ${base}`);
  }
}

/**
 * Compiles `src/index.css` exactly as the app build does, for the classes
 * `components/markdown.tsx` puts on its container.
 *
 * `tailwindcss`'s own `compile` is used rather than the bundler plugin so the
 * test depends only on what `package.json` declares. It resolves `@import` and
 * `@plugin` through Node, which is all this stylesheet needs.
 */
async function compileConsoleCss(): Promise<string> {
  const compiler = await compile(readFileSync(resolve(frontendRoot, "src/index.css"), "utf8"), {
    base: frontendRoot,
    loadStylesheet: async (id, base) => {
      const path = resolveStylesheet(id, base);
      return { path, base: dirname(path), content: readFileSync(path, "utf8") };
    },
    loadModule: async (id, base) => {
      const path = require.resolve(id, { paths: [base, frontendRoot] });
      const module = await import(path);
      return { path, base: dirname(path), module: module.default ?? module };
    },
  });
  return compiler.build(["prose", "prose-sm", "max-w-none", "dark:prose-invert"]);
}

/**
 * Inline code in the three places it lands in practice, plus a fenced block.
 * `remark-gfm` is what makes the table a table.
 */
const DOC = [
  "A reply that names `UsageDto` mid-sentence.",
  "",
  "- a list item mentioning `--company`",
  "",
  "| field | type |",
  "| --- | --- |",
  "| `usage` | `UsageDto` |",
  "",
  "```ts",
  "const usage: UsageDto = load();",
  "```",
].join("\n");

let rules: Rule[];
let container: HTMLDivElement;
let root: Root;

beforeAll(async () => {
  rules = flatten(await compileConsoleCss());
});

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(createElement(Markdown, { children: DOC }));
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

/** Every inline `<code>` — the ones outside a fenced block. */
function inlineCode(): HTMLElement[] {
  return [...container.querySelectorAll("code")].filter((code) => !code.closest("pre"));
}

function blockCode(): HTMLElement {
  const code = container.querySelector("pre code");
  if (!code) throw new Error("the fenced block did not render as `pre code`");
  return code as HTMLElement;
}

describe("inline code carries no backtick", () => {
  it("renders one inline span per mention, in prose, a list and a table", () => {
    // The premise the rest of the file rests on: these really are `<code>`
    // elements under `.prose`, so the plugin's defaults really do reach them.
    expect(inlineCode().map((code) => code.textContent)).toEqual([
      "UsageDto",
      "--company",
      "usage",
      "UsageDto",
    ]);
    expect(container.querySelector(".prose")).not.toBeNull();
    expect(container.querySelector("table")).not.toBeNull();
  });

  it("draws nothing before or after an inline span", () => {
    for (const code of inlineCode()) {
      for (const pseudo of ["::before", "::after"]) {
        expect(winner(rules, code, "content", pseudo), `${code.textContent} ${pseudo}`).toBe("none");
      }
    }
  });

  it("leaves no backtick in the text either", () => {
    // The other half of the same promise: the parser consumed the fences, so
    // nothing downstream may put them back as characters.
    for (const code of inlineCode()) {
      expect(code.textContent).not.toContain("`");
    }
    expect(container.textContent).not.toContain("`");
  });

  it("still ships the plugin default this overrides", () => {
    // If the plugin ever drops the backticks itself, this fails and the
    // override — and this test — can go. It failing is good news, not a bug.
    const injected = rules.filter(
      (rule) => rule.declarations.content === '"`"' && rule.selector.includes("code"),
    );
    expect(injected.length).toBeGreaterThan(0);
  });
});

describe("inline code reads as code", () => {
  it("gets a themed ground and padding of its own", () => {
    // Tokens, not values: `.dark` re-declares both, so the chip themes with
    // the rest of the console.
    for (const code of inlineCode()) {
      expect(winner(rules, code, "background-color")).toBe("var(--muted)");
      expect(winner(rules, code, "padding")).toBe("0.125em 0.375em");
    }
  });
});

describe("fenced blocks are left alone", () => {
  it("keeps the block's own ground rather than a chip per block", () => {
    // The inline rule is unlayered, so without its `pre *` exclusion it would
    // outrank the plugin's `pre code { background-color: transparent }` and
    // paint a second, bordered ground inside every code block.
    expect(winner(rules, blockCode(), "background-color")).toBe("transparent");
    expect(winner(rules, blockCode(), "padding")).toBe("0");
  });

  it("draws nothing before or after a block either", () => {
    for (const pseudo of ["::before", "::after"]) {
      expect(winner(rules, blockCode(), "content", pseudo)).toBe("none");
    }
  });
});
