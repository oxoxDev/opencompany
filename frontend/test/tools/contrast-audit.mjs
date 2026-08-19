/*
 * Contrast audit for the running console.
 *
 * Two mistakes in the first version are worth keeping in the comments,
 * because both produced a confident PASS while measuring almost nothing:
 *
 *   1. Colours were parsed with an rgb-only regex. getComputedStyle returns
 *      `oklch(...)` for every token in this app, so nearly every element was
 *      skipped and backgrounds fell back to white.
 *   2. The canvas `fillStyle` round-trip does NOT normalise oklch — Chrome
 *      accepts and preserves it. The only reliable conversion is to actually
 *      rasterise a pixel and read it back.
 *
 * Hence `measured`: a run that checks nothing must report that it checked
 * nothing, rather than reporting success.
 */
import { chromium } from '@playwright/test';

const SP = process.env.SP;
const BASE = 'http://127.0.0.1:8137';
const EMAIL = 'harness-e2e@tinyhumans.ai';

const AUDIT = () => {
  // Rasterise-and-read: the one conversion that handles oklch, color-mix,
  // named colours and rgb alike, because the engine does the work.
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
    if (ctx.fillStyle === '#000' && !/^(#000|black|rgb\(0, ?0, ?0)/.test(s)) {
      cache.set(s, null);
      return null; // the engine rejected it
    }
    ctx.fillRect(0, 0, 1, 1);
    const d = ctx.getImageData(0, 0, 1, 1).data;
    const out = { r: d[0], g: d[1], b: d[2], a: d[3] / 255 };
    cache.set(s, out);
    return out;
  };

  const lin = (c) => { c /= 255; return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4); };
  const lum = (c) => 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b);
  const over = (fg, bg) => ({
    r: fg.r * fg.a + bg.r * (1 - fg.a),
    g: fg.g * fg.a + bg.g * (1 - fg.a),
    b: fg.b * fg.a + bg.b * (1 - fg.a),
    a: 1,
  });
  const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };

  const bgOf = (el) => {
    let node = el, acc = null;
    while (node && node !== document.documentElement) {
      const c = parse(getComputedStyle(node).backgroundColor);
      if (c && c.a > 0) { acc = acc ? over(acc, c) : c; if (acc.a >= 0.999) return acc; }
      node = node.parentElement;
    }
    const root = parse(getComputedStyle(document.documentElement).backgroundColor)
      || parse(getComputedStyle(document.body).backgroundColor)
      || { r: 255, g: 255, b: 255, a: 1 };
    return acc ? over(acc, root) : root;
  };

  const SKIP = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'SVG', 'PATH']);
  const out = [];
  let measured = 0;

  for (const el of document.querySelectorAll('body *')) {
    if (SKIP.has(el.tagName)) continue;
    if (el.children.length > 0) continue;
    const text = (el.textContent || '').trim();
    if (!text || text.length < 2) continue;
    const st = getComputedStyle(el);
    if (st.visibility === 'hidden' || st.display === 'none' || +st.opacity === 0) continue;
    const box = el.getBoundingClientRect();
    if (box.width < 4 || box.height < 4) continue;

    const fgRaw = parse(st.color);
    if (!fgRaw) continue;
    const bg = bgOf(el);
    // Inherited opacity dims the text against the same ground it sits on.
    let eff = fgRaw.a < 1 ? over(fgRaw, bg) : fgRaw;
    let node = el, op = 1;
    while (node && node !== document.documentElement) { op *= +getComputedStyle(node).opacity; node = node.parentElement; }
    if (op < 1) eff = over({ ...eff, a: op }, bg);

    const r = ratio(eff, bg);
    measured++;
    const px = parseFloat(st.fontSize);
    const bold = +st.fontWeight >= 700;
    const large = px >= 24 || (px >= 18.66 && bold);
    const need = large ? 3 : 4.5;
    if (r < need) {
      out.push({ text: text.slice(0, 40), ratio: +r.toFixed(2), need, px, op: +op.toFixed(2) });
    }
  }
  return { out, measured };
};

const VIEWS = ['overview', 'company', 'chat', 'ledgers', 'workspace', 'approvals', 'workflows', 'settings', 'memory'];

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
if (await skip.count()) { await skip.first().click(); await page.waitForTimeout(600); }

const findings = [];
let totalMeasured = 0;

for (const theme of ['light', 'dark']) {
  await page.evaluate((t) => localStorage.setItem('theme', t), theme);
  await page.reload({ waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(1200);
  const applied = await page.evaluate(() => document.documentElement.className);
  if (!applied.includes(theme)) throw new Error(`theme did not apply: wanted ${theme}, got "${applied}"`);

  for (const v of VIEWS) {
    await page.goto(`${BASE}/#/${v}`, { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1600);
    const res = await page.evaluate(AUDIT);
    totalMeasured += res.measured;
    if (res.out.length) findings.push({ theme, view: v, bad: res.out, measured: res.measured });
    if (SP) await page.screenshot({ path: `${SP}/final-${v}-${theme}.png` });
  }
}

console.log(`\n===== CONTRAST AUDIT =====`);
console.log(`${totalMeasured} text nodes measured across ${VIEWS.length} views x 2 themes`);
if (totalMeasured < 100) console.log('!! suspiciously few — treat this run as inconclusive');
if (!findings.length) {
  console.log('\nno text below its WCAG threshold');
} else {
  for (const f of findings) {
    console.log(`\n${f.view} / ${f.theme}  (${f.measured} measured, ${f.bad.length} failing)`);
    const seen = new Set();
    for (const b of f.bad) {
      const k = b.text + b.ratio;
      if (seen.has(k)) continue;
      seen.add(k);
      console.log(`   ${String(b.ratio).padStart(5)}:1 need ${b.need}  ${b.px}px  opacity=${b.op}  "${b.text}"`);
    }
  }
}
console.log('\nJS errors:', jsErrors.length ? [...new Set(jsErrors)].join(' | ') : 'none');
await browser.close();
