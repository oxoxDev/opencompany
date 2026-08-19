/*
 * Contrast audit of INTERACTIVE states.
 *
 * The default-state audit (contrast-audit.mjs) only ever sees what a view
 * renders at rest. Popovers, dropdowns, dialogs and hover/focus states draw on
 * different surfaces — `--popover` rather than `--card`, `--accent` under a
 * hovered menu row — and those are exactly the combinations a token change is
 * most likely to break, because nobody looks at them.
 *
 * Same colour handling as the default audit: rasterise a pixel, because
 * getComputedStyle returns oklch and neither a regex nor a canvas fillStyle
 * round-trip will convert it.
 */
import { chromium } from '@playwright/test';

const BASE = 'http://127.0.0.1:8137';
const EMAIL = 'harness-e2e@tinyhumans.ai';
const SP = process.env.SP;

const AUDIT = () => {
  const cvs = document.createElement('canvas');
  cvs.width = cvs.height = 1;
  const ctx = cvs.getContext('2d', { willReadFrequently: true });
  const cache = new Map();
  const parse = (s) => {
    if (!s || s === 'transparent') return { r: 0, g: 0, b: 0, a: 0 };
    if (cache.has(s)) return cache.get(s);
    ctx.clearRect(0, 0, 1, 1);
    ctx.fillStyle = '#000';
    ctx.fillStyle = s;
    if (ctx.fillStyle === '#000' && !/^(#000|black|rgb\(0, ?0, ?0)/.test(s)) { cache.set(s, null); return null; }
    ctx.fillRect(0, 0, 1, 1);
    const d = ctx.getImageData(0, 0, 1, 1).data;
    const out = { r: d[0], g: d[1], b: d[2], a: d[3] / 255 };
    cache.set(s, out);
    return out;
  };
  const lin = (c) => { c /= 255; return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4); };
  const lum = (c) => 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b);
  const over = (f, b) => ({ r: f.r * f.a + b.r * (1 - f.a), g: f.g * f.a + b.g * (1 - f.a), b: f.b * f.a + b.b * (1 - f.a), a: 1 });
  const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
  const bgOf = (el) => {
    let n = el, acc = null;
    while (n && n !== document.documentElement) {
      const c = parse(getComputedStyle(n).backgroundColor);
      if (c && c.a > 0) { acc = acc ? over(acc, c) : c; if (acc.a >= 0.999) return acc; }
      n = n.parentElement;
    }
    const root = parse(getComputedStyle(document.documentElement).backgroundColor) || { r: 255, g: 255, b: 255, a: 1 };
    return acc ? over(acc, root) : root;
  };
  const SKIP = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'SVG', 'PATH']);
  const out = [];
  let measured = 0;
  for (const el of document.querySelectorAll('body *')) {
    if (SKIP.has(el.tagName) || el.children.length > 0) continue;
    const text = (el.textContent || '').trim();
    if (!text || text.length < 2) continue;
    const st = getComputedStyle(el);
    if (st.visibility === 'hidden' || st.display === 'none' || +st.opacity === 0) continue;
    const box = el.getBoundingClientRect();
    if (box.width < 4 || box.height < 4) continue;
    const fg = parse(st.color);
    if (!fg) continue;
    const bg = bgOf(el);
    let eff = fg.a < 1 ? over(fg, bg) : fg;
    let n = el, op = 1;
    while (n && n !== document.documentElement) { op *= +getComputedStyle(n).opacity; n = n.parentElement; }
    if (op < 1) eff = over({ ...eff, a: op }, bg);
    const r = ratio(eff, bg);
    measured++;
    const px = parseFloat(st.fontSize);
    const need = (px >= 24 || (px >= 18.66 && +st.fontWeight >= 700)) ? 3 : 4.5;
    if (r < need) out.push({ text: text.slice(0, 40), ratio: +r.toFixed(2), need, px });
  }
  return { out, measured };
};

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 950 }, deviceScaleFactor: 1.5 });
const api = ctx.request;
// Assert the sign-in, do not assume it.
//
// Destructuring `dev_code` from an unchecked body yields `undefined` on any
// failure, the verify call proceeds anyway, and the audit then measures the
// signed-out login screen — a page with almost no text, which passes. That is
// the false PASS this file's header is about, reached by a different route.
const requested = await api.post(`${BASE}/api/v1/company/auth/request`, { data: { email: EMAIL } });
if (!requested.ok()) {
  throw new Error(`auth/request failed: ${requested.status()} ${await requested.text()}`);
}
const { dev_code: devCode } = await requested.json();
if (!devCode) {
  throw new Error(
    'auth/request returned no dev_code. The host echoes one only when no mail is ' +
      'configured; without it this tool cannot sign in and must not report a pass.',
  );
}
const verified = await api.post(`${BASE}/api/v1/company/auth/verify`, {
  data: { email: EMAIL, code: devCode },
});
if (!verified.ok()) {
  throw new Error(`auth/verify failed: ${verified.status()} ${await verified.text()}`);
}
const page = await ctx.newPage();
const jsErrors = [];
page.on('pageerror', (e) => jsErrors.push(e.message));

await page.goto(`${BASE}/#/overview`, { waitUntil: 'domcontentloaded' });
await page.waitForTimeout(1800);
const skip = page.getByRole('button', { name: /skip for now/i });
if (await skip.count()) { await skip.first().click(); await page.waitForTimeout(500); }

/* Each scenario opens a surface the resting audit never sees. `settle` lets an
   entry animation finish — measuring mid-fade reads a transparent background
   and invents a failure. */
const SCENARIOS = [
  { name: 'settings: theme dropdown', view: 'settings', act: async (p) => {
      const t = p.getByRole('button', { name: /change theme|theme/i });
      if (await t.count()) { await t.first().click(); return true; } return false; } },
  { name: 'company: new desk dialog', view: 'company', act: async (p) => {
      const t = p.getByRole('button', { name: /new desk/i });
      if (await t.count()) { await t.first().click(); return true; } return false; } },
  { name: 'company: add teammate', view: 'company', act: async (p) => {
      const t = p.getByRole('button', { name: /add teammate/i });
      if (await t.count()) { await t.first().click(); return true; } return false; } },
  { name: 'workflows: any dialog', view: 'workflows', act: async (p) => {
      const t = p.getByRole('button', { name: /new workflow|create/i });
      if (await t.count()) { await t.first().click(); return true; } return false; } },
  { name: 'overview: sidebar hover', view: 'overview', act: async (p) => {
      const t = p.getByRole('link', { name: /join our discord/i });
      if (await t.count()) { await t.first().hover(); return true; } return false; } },
  { name: 'overview: keyboard focus ring', view: 'overview', act: async (p) => {
      await p.keyboard.press('Tab'); await p.keyboard.press('Tab'); return true; } },
  { name: 'ledgers: board', view: 'ledgers/tasks', act: async () => true },
  { name: 'chat: composer focus', view: 'chat', act: async (p) => {
      const t = p.locator('textarea, input[type=text]').first();
      if (await t.count()) { await t.focus(); return true; } return false; } },
];

let total = 0;
const findings = [];
for (const theme of ['light', 'dark']) {
  await page.evaluate((t) => localStorage.setItem('theme', t), theme);
  await page.reload({ waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(1200);
  const applied = await page.evaluate(() => document.documentElement.className);
  if (!applied.includes(theme)) throw new Error(`theme did not apply: ${applied}`);

  for (const s of SCENARIOS) {
    await page.goto(`${BASE}/#/${s.view}`, { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1400);
    let opened = false;
    try { opened = await s.act(page); } catch { opened = false; }
    await page.waitForTimeout(700); // settle
    const res = await page.evaluate(AUDIT);
    total += res.measured;
    if (SP) await page.screenshot({ path: `${SP}/act-${s.name.replace(/[^a-z0-9]+/gi, '-')}-${theme}.png` });
    if (res.out.length) findings.push({ theme, name: s.name, opened, bad: res.out, measured: res.measured });
    await page.keyboard.press('Escape').catch(() => {});
  }
}

console.log(`\n===== INTERACTIVE STATE AUDIT =====`);
console.log(`${total} text nodes measured across ${SCENARIOS.length} scenarios x 2 themes`);
if (!findings.length) console.log('\nno text below its WCAG threshold in any opened state');
for (const f of findings) {
  console.log(`\n${f.name} / ${f.theme}  (opened=${f.opened}, ${f.measured} measured)`);
  const seen = new Set();
  for (const b of f.bad) {
    const k = b.text + b.ratio; if (seen.has(k)) continue; seen.add(k);
    console.log(`   ${String(b.ratio).padStart(5)}:1 need ${b.need}  ${b.px}px  "${b.text}"`);
  }
}
console.log('\nJS errors:', jsErrors.length ? [...new Set(jsErrors)].join(' | ') : 'none');
await browser.close();
