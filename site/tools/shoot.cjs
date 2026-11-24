// Renders the page into shots/: 1440 and 390 px, light and dark, full page and first screen,
// the simulator in use, and the contents panel on a phone. 360 px is checked for overflow only.
// Usage: node tools/shoot.cjs [--fold]
const path = require('path');
const { chromium } = require(path.join(__dirname, '..', '..', 'e2e', 'node_modules', '@playwright', 'test'));

const root = path.resolve(__dirname, '..');
const url = 'file://' + path.join(root, 'index.html');
const fold = process.argv.includes('--fold');
const out = (n) => path.join(root, 'shots', n + '.png');

async function settle(page) {
  await page.evaluate(async () => {
    document.querySelectorAll('img[loading="lazy"]').forEach((i) => { i.loading = 'eager'; });
    await document.fonts.ready;
    await Promise.all([...document.images].map((i) => i.complete ? 0 : new Promise((r) => { i.onload = i.onerror = r; })));
  });
  for (let y = 0; y < await page.evaluate(() => document.body.scrollHeight); y += 700) {
    await page.evaluate((yy) => window.scrollTo(0, yy), y);
    await page.waitForTimeout(40);
  }
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.waitForTimeout(250);
}

async function overflow(page) {
  return page.evaluate(() => {
    const W = document.documentElement.clientWidth;
    const bad = [];
    document.querySelectorAll('body *').forEach((e) => {
      const r = e.getBoundingClientRect();
      if (!r.width || e.closest('pre, .foot-word, .shot picture, details:not([open])')) return;
      if ((r.right > W + 1 || r.left < -1) && getComputedStyle(e).position !== 'absolute') bad.push(`${e.tagName}.${e.className} ${Math.round(r.left)}..${Math.round(r.right)}`);
    });
    return { scrollW: document.documentElement.scrollWidth, W, bad: bad.slice(0, 12) };
  });
}

(async () => {
  const browser = await chromium.launch();
  const external = [];
  for (const [w, h, name] of [[1440, 900, 'desktop'], [390, 844, 'mobile'], [360, 740, 'narrow']]) {
    for (const scheme of ['light', 'dark']) {
      const ctx = await browser.newContext({ viewport: { width: w, height: h }, colorScheme: scheme, reducedMotion: 'reduce', deviceScaleFactor: name === 'desktop' ? 1 : 2 });
      const page = await ctx.newPage();
      page.on('request', (r) => { if (!r.url().startsWith('file:') && !r.url().startsWith('data:')) external.push(r.url()); });
      page.on('pageerror', (e) => console.log('pageerror', e.message));
      page.on('console', (m) => { if (m.type() === 'error') console.log('console', m.text()); });
      await page.goto(url);
      await settle(page);
      console.log(name, scheme, JSON.stringify(await overflow(page)));
      if (name === 'narrow') { await ctx.close(); continue; }
      await page.screenshot({ path: out(`${name}-${scheme}-fold`) });
      if (!fold) {
        await page.screenshot({ path: out(`${name}-${scheme}`), fullPage: true });
        // the simulator in use: an old release stays fixed, a new one reopens
        await page.click('.rel[data-rel="2.4.1"]');
        await page.click('.rel[data-rel="2.6.0"]');
        await page.waitForTimeout(150);
        await page.locator('.sim').screenshot({ path: out(`sim-${name}-${scheme}`) });
        await page.click('.fix[data-fix=""]');
        await page.click('.rel[data-rel="2.6.0"]');
        await page.click('.rel[data-rel=""]');
        await page.waitForTimeout(150);
        await page.locator('.sim').screenshot({ path: out(`sim2-${name}-${scheme}`) });
        if (name === 'mobile') {
          await page.evaluate(() => window.scrollTo(0, 0));
          await page.click('.toc summary');
          await page.waitForTimeout(150);
          await page.screenshot({ path: out(`toc-${name}-${scheme}`) });
        }
      }
      await ctx.close();
    }
  }
  console.log('external requests:', external.length ? external : 'none');
  await browser.close();
})();
