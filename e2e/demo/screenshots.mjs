// Takes the README screenshots from a Davy that e2e/demo/seed.mjs filled.
// Usage: node e2e/demo/screenshots.mjs WELCOME_LINK OUT_DIR
import { chromium } from "@playwright/test";

const [link, out] = process.argv.slice(2);
if (!link || !out) throw new Error("usage: node e2e/demo/screenshots.mjs WELCOME_LINK OUT_DIR");
const base = new URL(link).origin;
const PASSWORD = "correct horse battery staple";

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
const page = await context.newPage();
page.on("pageerror", (e) => {
  throw e;
});

await page.goto(link);
await page.fill("#password", PASSWORD);
await page.fill("#again", PASSWORD);
await page.click("button[type=submit]");
await page.waitForURL(/\/ui\/(?!welcome)/);

const views = [
  ["issues", "/ui/issues", "main"],
  ["issue", "/ui/issues/1", "main"],
  ["logs", "/ui/logs?range=24h", "main"],
];
for (const scheme of ["light", "dark"]) {
  await page.emulateMedia({ colorScheme: scheme, reducedMotion: "reduce" });
  for (const [name, path, ready] of views) {
    await page.goto(`${base}${path}`);
    await page.waitForSelector(ready);
    await page.waitForLoadState("networkidle");
    await page.waitForFunction(() => document.getAnimations().length === 0);
    await page.screenshot({ path: `${out}/${name}-${scheme}.png` });
  }
}
await browser.close();
console.log(`screenshots: wrote ${views.length * 2} files to ${out}`);
